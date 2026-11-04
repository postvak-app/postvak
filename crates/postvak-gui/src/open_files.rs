//! Attach…: the desktop's open dialog, where the user picks the files. On Linux the FileChooser portal, which also
//! reaches out of a Flatpak, on a thread of its own since it waits for the user; on macOS an `NSOpenPanel`, which
//! AppKit runs on the main thread, where it is modal.
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::html::Wake;

/// The files chosen, none when the user cancelled.
pub(crate) type Chosen = anyhow::Result<Vec<PathBuf>>;

/// Opens the open dialog for one or more files. The answer arrives on the receiver, and `wake` runs once it has.
pub(crate) fn ask(wake: Wake) -> Receiver<Chosen> {
    #[cfg(target_os = "linux")]
    {
        crate::worker::oneshot("open-files", wake, || {
            postvak_core::portal::open_files("Attach files")
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (done, answer) = std::sync::mpsc::channel();
        #[cfg(target_os = "macos")]
        let _ = done.send(mac::choose());
        #[cfg(not(target_os = "macos"))]
        let _ = done.send(Err(anyhow::anyhow!("Attach works on macOS and Linux")));
        wake();
        answer
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use anyhow::Context;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};

    use super::Chosen;

    /// Runs the open panel modally; the window's own thread is the main thread.
    pub(super) fn choose() -> Chosen {
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
