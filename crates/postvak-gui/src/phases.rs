//! How long the window takes to show a list: with `POSTVAK_STARTUP_METRICS=1`, one JSON line per startup phase on
//! stderr, `{"startup":"<phase>","ms":<ms since start>}`, which `scripts/startup.py` reads (see docs/src/perf.md).
//! Silent otherwise.
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// The variable that turns the lines on.
const VAR: &str = "POSTVAK_STARTUP_METRICS";

/// When this process started measuring; None when it does not measure.
static CLOCK: OnceLock<Option<Instant>> = OnceLock::new();
static PHASES: Mutex<Phases> = Mutex::new(Phases { seen: Vec::new() });

/// Starts the clock when `POSTVAK_STARTUP_METRICS` asks for it, as the `start` phase at 0 ms.
pub(crate) fn start() {
    mark("start");
}

/// Reports `phase` once, with the time since `start`.
pub(crate) fn mark(phase: &'static str) {
    let clock =
        CLOCK.get_or_init(|| enabled(std::env::var(VAR).ok().as_deref()).then(Instant::now));
    let Some(started) = clock else { return };
    let line = PHASES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .line_once(phase, started.elapsed());
    if let Some(line) = line {
        eprintln!("{line}");
    }
}

fn enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0")
}

/// The phases reported so far: a phase that comes again, such as a second window's first frame, is not.
struct Phases {
    seen: Vec<&'static str>,
}

impl Phases {
    fn line_once(&mut self, phase: &'static str, elapsed: Duration) -> Option<String> {
        if self.seen.contains(&phase) {
            return None;
        }
        self.seen.push(phase);
        let ms = (elapsed.as_secs_f64() * 1_000_000.0).round() / 1000.0;
        Some(serde_json::json!({ "startup": phase, "ms": ms }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_is_one_json_line_in_milliseconds() {
        let mut phases = Phases { seen: Vec::new() };
        let line = phases
            .line_once("first_frame", Duration::from_micros(12_345))
            .unwrap();
        assert!(!line.contains('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!({ "startup": "first_frame", "ms": 12.345 })
        );
    }

    #[test]
    fn a_phase_is_reported_once() {
        let mut phases = Phases { seen: Vec::new() };
        assert!(phases.line_once("start", Duration::ZERO).is_some());
        assert!(phases.line_once("start", Duration::from_secs(1)).is_none());
        assert!(
            phases
                .line_once("config_loaded", Duration::from_secs(1))
                .is_some()
        );
    }

    #[test]
    fn only_a_set_variable_turns_it_on() {
        assert!(!enabled(None));
        assert!(!enabled(Some("")));
        assert!(!enabled(Some("0")));
        assert!(enabled(Some("1")));
    }
}
