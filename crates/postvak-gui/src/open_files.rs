//! Attach…: the desktop's open dialog, where the user picks the files. On Linux the FileChooser portal, which also
//! reaches out of a Flatpak, on a thread of its own since it waits for the user; on macOS an `NSOpenPanel`, which
//! AppKit runs on the main thread, where it is modal. The files' sizes are read on a worker, never the window's thread.
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::html::Wake;

/// The files chosen with their sizes, None for one that could not be read; none when the user cancelled.
pub(crate) type Chosen = anyhow::Result<Vec<(PathBuf, Option<u64>)>>;

/// Opens the open dialog for one or more files. The answer arrives on the receiver, and `wake` runs once it has.
pub(crate) fn ask(wake: Wake) -> Receiver<Chosen> {
    #[cfg(target_os = "linux")]
    let chosen = || postvak_core::portal::open_files("Attach files");
    #[cfg(target_os = "macos")]
    let chosen = {
        let paths = mac::choose();
        move || paths
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let chosen = || Err(anyhow::anyhow!("Attach works on macOS and Linux"));
    crate::worker::oneshot("open-files", wake, move || chosen().map(sized))
}

/// Each path with its size, read now, once.
pub(crate) fn sized(paths: Vec<PathBuf>) -> Vec<(PathBuf, Option<u64>)> {
    paths
        .into_iter()
        .map(|path| {
            let size = std::fs::metadata(&path).ok().map(|m| m.len());
            (path, size)
        })
        .collect()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::PathBuf;

    use anyhow::Context;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};

    /// Runs the open panel modally; the window's own thread is the main thread.
    pub(super) fn choose() -> anyhow::Result<Vec<PathBuf>> {
        let main =
            MainThreadMarker::new().context("the open panel opens from the main thread only")?;
        let panel = NSOpenPanel::openPanel(main);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(true);
        if panel.runModal() != NSModalResponseOK {
            return Ok(Vec::new());
        }
        Ok(panel
            .URLs()
            .iter()
            .filter_map(|url| url.to_file_path())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_that_cannot_be_read_has_no_size() {
        let dir = tempfile::tempdir().unwrap();
        let there = dir.path().join("there.txt");
        std::fs::write(&there, "12345").unwrap();
        let gone = dir.path().join("gone.txt");
        assert_eq!(
            super::sized(vec![there.clone(), gone.clone()]),
            [(there, Some(5)), (gone, None)]
        );
    }
}
