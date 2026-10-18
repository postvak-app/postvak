//! Crash reports, written to the crash folder and never sent anywhere. They hold no mail data: actions by variant
//! name, thread names up to their first non-word character, and panic messages only when they are literals.
use std::backtrace::Backtrace;
use std::fmt::{self, Write as _};
use std::io;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::paths;

/// Older reports are removed past this many.
const KEEP: usize = 20;

static LAST_ACTION: Mutex<String> = Mutex::new(String::new());

/// Writes a report to `dir` on every panic, then runs the hook that was there before.
pub fn install(dir: PathBuf, process: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let id = format!(
            "{}-{process}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f")
        );
        if let Err(e) = write(&dir, &id, &report(info, process)) {
            log::warn!("could not write a crash report to {}: {e}", dir.display());
        }
        previous(info);
    }));
}

/// Records the kind of the `Command` or `UiAction` being carried out, for the next report.
pub fn note(action: &impl fmt::Debug) {
    let mut kind = Word(String::new());
    // Word stops the formatting at the end of the variant name, so the fields are never formatted.
    let _ = write!(kind, "{action:?}");
    if let Ok(mut last) = LAST_ACTION.lock() {
        *last = kind.0;
    }
}

/// The reports in `dir`, newest first, by id.
pub fn list(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".txt")) {
            ids.push(id.to_string());
        }
    }
    ids.sort_unstable_by(|a, b| b.cmp(a));
    Ok(ids)
}

/// The report `id`, as `list` names it.
pub fn show(dir: &Path, id: &str) -> io::Result<String> {
    if !list(dir)?.iter().any(|known| known == id) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no crash report {id}; `postvak crashes` lists them"),
        ));
    }
    std::fs::read_to_string(dir.join(format!("{id}.txt")))
}

fn report(info: &PanicHookInfo, process: &str) -> String {
    let thread = std::thread::current();
    let thread = first_word(thread.name().unwrap_or("unnamed"));
    // try_lock: a panic while `note` holds the lock must not deadlock the hook.
    let last = LAST_ACTION
        .try_lock()
        .map(|last| last.clone())
        .unwrap_or_default();
    let location = info
        .location()
        .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
        .unwrap_or_default();
    let message = info
        .payload()
        .downcast_ref::<&'static str>()
        .copied()
        .unwrap_or("(withheld: a formatted message can quote mail)");
    format!(
        "Postvak crash report. It is never sent anywhere; attach it to a bug report if you like.\n\
         version: {}\nos: {} {}\nprocess: {process}\nthread: {thread}\nlast action: {last}\n\
         location: {location}\nmessage: {message}\n\nbacktrace:\n{}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        Backtrace::force_capture(),
    )
}

fn write(dir: &Path, id: &str, report: &str) -> io::Result<()> {
    paths::create_private_dir(dir)?;
    paths::write_atomic(&dir.join(format!("{id}.txt")), report.as_bytes())?;
    prune(dir, KEEP)
}

fn prune(dir: &Path, keep: usize) -> io::Result<()> {
    for id in list(dir)?.iter().skip(keep) {
        std::fs::remove_file(dir.join(format!("{id}.txt")))?;
    }
    Ok(())
}

fn first_word(text: &str) -> &str {
    let end = text.find(|c: char| !is_word(c)).unwrap_or(text.len());
    &text[..end]
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Keeps the first word written to it, then fails the formatting.
struct Word(String);

impl fmt::Write for Word {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let word = first_word(text);
        self.0.push_str(word);
        if word.len() < text.len() {
            return Err(fmt::Error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_newest_reports_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..5 {
            std::fs::write(dir.path().join(format!("2026010{n}-gui.txt")), "").unwrap();
        }
        prune(dir.path(), 2).unwrap();
        assert_eq!(list(dir.path()).unwrap(), ["20260104-gui", "20260103-gui"]);
    }

    #[test]
    fn a_noted_action_keeps_its_variant_name_only() {
        #[derive(Debug)]
        #[allow(dead_code)]
        enum Action {
            Move(String),
        }
        note(&Action::Move("jane@example.org".into()));
        assert_eq!(*LAST_ACTION.lock().unwrap(), "Move");
    }
}
