#!/usr/bin/env python3
"""How long the window takes to show a list, and how much memory the GUI and the daemon use: see docs/src/perf.md.

    scripts/startup.py run [--bin target/release/postvak] [--scale 10000] [--runs 3] [--out startup.json]
    scripts/startup.py show startup.json
    scripts/startup.py compare old.json new.json

`run` writes a mail home of generated mail (the benchmark's generator in crates/postvak-gui/src/bench.rs, so no user
data is involved), starts the GUI on a fresh copy of it `--runs` times and records, per run:

- the startup phases the GUI prints with POSTVAK_STARTUP_METRICS=1, in ms since it started measuring (`ms`) and since
  this script started the process (`wall_ms`);
- RSS, PSS and USS of the GUI and of the daemon it starts, from /proc/<pid>/smaps_rollup on Linux; RSS from `ps` and
  the physical footprint from `footprint` on macOS. Once idle, with an HTML newsletter open, and after another
  message replaced it, which catches a renderer that never frees.

It runs on whatever display and renderer the environment gives the GUI: set SLINT_BACKEND=winit-software for the
software renderer. Python 3 standard library only.
"""

import argparse
import datetime
import json
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# The messages bench::write_home adds to the `home` account's INBOX.
NEWSLETTER_UID = 1_000_000
PLAIN_UID = 1_000_001
PHASES = ["start", "backend_selected", "config_loaded", "daemon_connected", "stores_opened", "first_list", "first_frame"]
POINTS = ["idle", "newsletter", "closed"]
PROCESSES = ["gui", "daemon"]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="measure and write a report")
    run.add_argument("--bin", type=Path, default=ROOT / "target/release/postvak", help="the postvak binary")
    run.add_argument("--home", type=Path, help="a mail home written before, instead of generating one")
    run.add_argument("--scale", type=int, default=10_000, help="messages over all accounts (default 10000)")
    run.add_argument("--runs", type=int, default=3, help="GUI starts to take the median of (default 3)")
    run.add_argument("--settle", type=float, default=3.0, help="seconds to wait before each memory reading")
    run.add_argument("--timeout", type=float, default=60.0, help="seconds to wait for the first frame")
    run.add_argument("--out", type=Path, help="where to write the report (default: stdout)")
    show = commands.add_parser("show", help="print a report as Markdown tables")
    show.add_argument("report", type=Path)
    compare = commands.add_parser("compare", help="print two reports side by side as a Markdown table")
    compare.add_argument("old", type=Path)
    compare.add_argument("new", type=Path)
    args = parser.parse_args()
    if args.command == "run":
        report = measure(args)
        text = json.dumps(report, indent=2) + "\n"
        if args.out:
            args.out.write_text(text)
        else:
            sys.stdout.write(text)
    elif args.command == "show":
        print(markdown(load(args.report)))
    else:
        print(comparison(load(args.old), load(args.new)))


def load(path):
    return json.loads(path.read_text())


def measure(args):
    if not args.bin.exists():
        sys.exit(f"no binary at {args.bin}; build it with `cargo build --release`")
    with tempfile.TemporaryDirectory(prefix="postvak-startup-") as scratch:
        scratch = Path(scratch)
        home = args.home or generate(scratch / "generated", args.scale)
        runs = []
        for number in range(args.runs):
            copy = scratch / f"run-{number}"
            shutil.copytree(home, copy)
            runs.append(one_run(args, copy))
            print(f"run {number + 1} of {args.runs} done", file=sys.stderr)
    return {
        "schema": 1,
        "created": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "commit": git_commit(),
        "machine": {
            "os": platform.system(),
            "release": platform.release(),
            "arch": platform.machine(),
            "cpus": os.cpu_count(),
        },
        "renderer": os.environ.get("SLINT_BACKEND") or "default",
        "display": display(),
        "messages": None if args.home else args.scale,
        "settle_s": args.settle,
        "startup": summarize_startup(runs),
        "memory": summarize_memory(runs),
        "runs": runs,
    }


def generate(home, scale):
    """Writes the mail home through the ignored test bench::home, in a release build like the binary's."""
    print(f"generating {scale} messages into {home}", file=sys.stderr)
    env = dict(os.environ, POSTVAK_PERF_HOME=str(home), POSTVAK_BENCH_SCALE=str(scale))
    subprocess.run(
        ["cargo", "test", "--release", "-p", "postvak-gui", "--lib", "bench::home", "--", "--ignored", "--exact"],
        cwd=ROOT,
        env=env,
        check=True,
        stdout=sys.stderr,
    )
    return home


def one_run(args, home):
    env = dict(os.environ, POSTVAK_HOME=str(home), POSTVAK_STARTUP_METRICS="1")
    # A start from a desktop entry would hand its token on; this one has none.
    for var in ("XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"):
        env.pop(var, None)
    started = time.monotonic()
    gui = subprocess.Popen(
        [str(args.bin), "gui"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
    )
    phases = {}
    log = []
    arrived = threading.Event()

    def read_stderr():
        for raw in gui.stderr:
            line = raw.decode(errors="replace").rstrip("\n")
            wall = (time.monotonic() - started) * 1000
            try:
                parsed = json.loads(line)
            except ValueError:
                parsed = None
            if isinstance(parsed, dict) and "startup" in parsed:
                phases[parsed["startup"]] = {"ms": parsed["ms"], "wall_ms": round(wall, 3)}
                if parsed["startup"] == "first_frame":
                    arrived.set()
            else:
                log.append(line)

    reader = threading.Thread(target=read_stderr, daemon=True)
    reader.start()
    daemon_pid = None

    def alive(what):
        if gui.poll() is not None:
            reader.join(5)
            tail = "\n".join(log[-30:])
            sys.exit(f"the GUI exited {what} (status {gui.returncode}); its last lines:\n{tail}")

    try:
        deadline = started + args.timeout
        while not arrived.wait(0.05):
            alive("before its first frame")
            if time.monotonic() > deadline:
                sys.exit(f"the GUI showed no frame in {args.timeout} s")
        daemon_pid = read_pid(home / "state" / "daemon.lock")
        memory = {}
        time.sleep(args.settle)
        alive("while idle")
        memory["idle"] = sample(gui.pid, daemon_pid)
        for point, uid in (("newsletter", NEWSLETTER_UID), ("closed", PLAIN_UID)):
            # A second start hands the open window the message and exits.
            subprocess.run(
                [str(args.bin), "gui", "--open", str(uid), "--account", "home", "--folder", "INBOX"],
                env=dict(env, POSTVAK_STARTUP_METRICS="0"),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=True,
                timeout=args.timeout,
            )
            time.sleep(args.settle)
            alive(f"after opening message {uid}")
            memory[point] = sample(gui.pid, daemon_pid)
    finally:
        stop(gui)
        subprocess.run(
            [str(args.bin), "daemon", "stop"],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )
        if daemon_pid:
            wait_gone(daemon_pid)
    missing = [phase for phase in PHASES if phase not in phases]
    if missing:
        sys.exit(f"the GUI did not report {', '.join(missing)}")
    return {"phases": phases, "memory": memory}


def stop(process):
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try:
            process.wait(10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def wait_gone(pid, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        except PermissionError:
            return
        time.sleep(0.1)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def read_pid(lock, seconds=10):
    """The daemon writes its pid into its lock file once it holds the lock."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            return int(lock.read_text().strip())
        except (OSError, ValueError):
            time.sleep(0.05)
    sys.exit(f"no daemon pid in {lock}")


def sample(gui_pid, daemon_pid):
    return {"gui": memory_of(gui_pid), "daemon": memory_of(daemon_pid)}


def memory_of(pid):
    """Sizes in kB; a size the platform does not give is None."""
    if sys.platform.startswith("linux"):
        fields = {}
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
            name, _, rest = line.partition(":")
            value = rest.split()
            if len(value) == 2 and value[1] == "kB":
                fields[name] = int(value[0])
        return {
            "rss_kb": fields.get("Rss"),
            "pss_kb": fields.get("Pss"),
            "uss_kb": fields.get("Private_Clean", 0) + fields.get("Private_Dirty", 0),
        }
    if sys.platform == "darwin":
        rss = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
        return {"rss_kb": int(rss) if rss.isdigit() else None, "footprint_kb": footprint(pid)}
    sys.exit(f"no memory reading on {sys.platform}")


def footprint(pid):
    """The physical footprint `footprint` reports, which is what Activity Monitor shows as Memory."""
    result = subprocess.run(["footprint", str(pid)], capture_output=True, text=True)
    found = re.search(r"Footprint:\s*([\d.]+)\s*(B|KB|MB|GB)", result.stdout)
    if not found:
        return None
    scale = {"B": 1 / 1024, "KB": 1, "MB": 1024, "GB": 1024 * 1024}[found.group(2)]
    return round(float(found.group(1)) * scale)


def summarize_startup(runs):
    summary = {}
    for phase in PHASES:
        ms = [run["phases"][phase]["ms"] for run in runs]
        wall = [run["phases"][phase]["wall_ms"] for run in runs]
        summary[phase] = {
            "median_ms": statistics.median(ms),
            "min_ms": min(ms),
            "max_ms": max(ms),
            "median_wall_ms": statistics.median(wall),
        }
    return summary


def summarize_memory(runs):
    summary = {}
    for point in POINTS:
        summary[point] = {}
        for process in PROCESSES:
            metrics = runs[0]["memory"][point][process].keys()
            summary[point][process] = {
                metric: median_or_none([run["memory"][point][process][metric] for run in runs]) for metric in metrics
            }
    return summary


def median_or_none(values):
    values = [value for value in values if value is not None]
    return statistics.median(values) if values else None


def git_commit():
    result = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True)
    return result.stdout.strip() or None


def display():
    if os.environ.get("WAYLAND_DISPLAY"):
        return "wayland"
    if os.environ.get("DISPLAY"):
        return "x11"
    return "macos" if sys.platform == "darwin" else None


def metrics(report):
    """Every number of a report's summary, by a name such as `first_frame median_ms` or `idle gui pss_kb`."""
    flat = {}
    for phase, values in report["startup"].items():
        for name, value in values.items():
            flat[f"{phase} {name}"] = value
    for point, processes in report["memory"].items():
        for process, values in processes.items():
            for name, value in values.items():
                flat[f"{point} {process} {name}"] = value
    return flat


def describe(report):
    machine = report["machine"]
    return (
        f"{report['commit'] or 'unknown commit'}, {machine['os']} {machine['release']} {machine['arch']}, "
        f"{machine['cpus']} CPUs, renderer {report['renderer']}, display {report['display']}, "
        f"{report['messages'] or 'given'} messages, {len(report['runs'])} runs"
    )


def number(value):
    if value is None:
        return "–"
    return f"{value:,.1f}" if isinstance(value, float) and not value.is_integer() else f"{value:,.0f}"


def markdown(report):
    lines = [describe(report), "", "| Phase | Median ms | Min | Max | Since spawn |", "| --- | --- | --- | --- | --- |"]
    for phase, values in report["startup"].items():
        lines.append(
            f"| `{phase}` | {number(values['median_ms'])} | {number(values['min_ms'])} | "
            f"{number(values['max_ms'])} | {number(values['median_wall_ms'])} |"
        )
    names = sorted({name for p in report["memory"].values() for v in p.values() for name in v})
    lines += ["", "| Point | Process | " + " | ".join(names) + " |", "| --- | --- |" + " --- |" * len(names)]
    for point, processes in report["memory"].items():
        for process, values in processes.items():
            cells = " | ".join(number(values.get(name)) for name in names)
            lines.append(f"| {point} | {process} | {cells} |")
    return "\n".join(lines)


def comparison(old, new):
    before, after = metrics(old), metrics(new)
    lines = [
        f"Old: {describe(old)}",
        f"New: {describe(new)}",
        "",
        "| Metric | Old | New | Change |",
        "| --- | --- | --- | --- |",
    ]
    for name in list(before) + [name for name in after if name not in before]:
        a, b = before.get(name), after.get(name)
        if a is None or b is None:
            change = "–"
        elif a == 0:
            change = number(b - a)
        else:
            change = f"{(b - a) / a * 100:+.1f}%"
        lines.append(f"| {name} | {number(a)} | {number(b)} | {change} |")
    return "\n".join(lines)


if __name__ == "__main__":
    main()
