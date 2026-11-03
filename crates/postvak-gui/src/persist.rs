//! The window's size, position, pane widths and composer split, kept in `window.json` in the state directory between runs. A missing
//! or unreadable file starts from the defaults; the startup error window keeps nothing.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use postvak_core::paths::Paths;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Layout {
    /// Logical pixels.
    pub width: f32,
    pub height: f32,
    /// Physical pixels on the screen; None where the platform does not say (Wayland).
    pub position: Option<(i32, i32)>,
    pub folders_width: f32,
    pub list_width: f32,
    /// The reader pane's composer, as its part of the pane's height.
    #[serde(default)]
    pub compose_height: Option<f32>,
}

pub(crate) fn path(paths: &Paths) -> PathBuf {
    paths.state_dir.join("window.json")
}

/// The saved layout, or None when there is none or it does not read.
pub(crate) fn load(path: &Path) -> Option<Layout> {
    let text = std::fs::read(path).ok()?;
    match serde_json::from_slice(&text) {
        Ok(layout) => Some(layout),
        Err(e) => {
            log::warn!("ignoring {}: {e}", path.display());
            None
        }
    }
}

pub(crate) fn save(path: &Path, layout: &Layout) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(layout)?;
    postvak_core::paths::write_atomic(path, &json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        let layout = Layout {
            width: 1100.0,
            height: 700.0,
            position: Some((40, 60)),
            folders_width: 260.0,
            list_width: 420.0,
            compose_height: Some(0.4),
        };
        save(&path, &layout).unwrap();
        assert_eq!(load(&path), Some(layout));
    }

    #[test]
    fn a_file_from_before_the_composer_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        let old = r#"{"width":900.0,"height":600.0,"position":null,"folders_width":200.0,"list_width":400.0}"#;
        std::fs::write(&path, old).unwrap();
        assert_eq!(load(&path).map(|layout| layout.compose_height), Some(None));
    }

    #[test]
    fn a_missing_or_corrupt_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        assert_eq!(load(&path), None);
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), None);
    }
}
