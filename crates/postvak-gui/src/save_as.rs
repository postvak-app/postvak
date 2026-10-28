//! Save as…: the desktop's save dialog, where the user picks the file. On Linux the FileChooser portal, which also
//! reaches out of a Flatpak, on a thread of its own since it waits for the user; on macOS an `NSSavePanel`, which
//! AppKit runs on the main thread, where it is modal.
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use crate::html::Wake;

/// The path chosen, or None when the user cancelled.
pub(crate) type Chosen = anyhow::Result<Option<PathBuf>>;

/// Opens the save dialog with `name` filled in, starting in `folder`. The answer arrives on the receiver, and `wake`
/// runs once it has.
pub(crate) fn ask(name: &str, folder: &Path, wake: Wake) -> Receiver<Chosen> {
    #[cfg(target_os = "linux")]
    {
        let (name, folder) = (name.to_string(), folder.to_path_buf());
        crate::worker::oneshot("save-as", wake, move || {
            postvak_core::portal::save_file("Save as", &name, Some(&folder))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (done, answer) = std::sync::mpsc::channel();
        #[cfg(target_os = "macos")]
        let _ = done.send(mac::choose(name, folder));
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (name, folder);
            let _ = done.send(Err(anyhow::anyhow!("Save as works on macOS and Linux")));
        }
        wake();
        answer
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::Path;

    use anyhow::Context;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSSavePanel};
    use objc2_foundation::{NSString, NSURL};

    use super::Chosen;

    /// Runs the save panel modally; the window's own thread is the main thread.
    pub(super) fn choose(name: &str, folder: &Path) -> Chosen {
        let main =
            MainThreadMarker::new().context("the save panel opens from the main thread only")?;
        let panel = NSSavePanel::savePanel(main);
        panel.setNameFieldStringValue(&NSString::from_str(name));
        if let Some(folder) = NSURL::from_directory_path(folder) {
            panel.setDirectoryURL(Some(&folder));
        }
        if panel.runModal() != NSModalResponseOK {
            return Ok(None);
        }
        Ok(panel.URL().and_then(|url| url.to_file_path()))
    }
}
