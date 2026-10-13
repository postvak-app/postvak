//! The grid and the light pane palette of `src/gui/theme.rs` (`PAPER`), as GPUI values.
use gpui::{Hsla, Pixels, px, rgb};

pub const PAD: Pixels = px(8.);
pub const INSET: Pixels = px(16.);
/// `list::LIST_ROW`: the sender and date, then the subject.
pub const LIST_ROW: Pixels = px(48.);
/// The list pane's width; the body takes the rest.
pub const LIST_WIDTH: Pixels = px(440.);

pub const TEXT_FONT: &str = "Hanken Grotesk";
pub const MONO_FONT: &str = "Spline Sans Mono";
pub const SERIF_FONT: &str = "Newsreader";

pub fn accent() -> Hsla {
    rgb(0x9a4a1c).into()
}

pub fn background() -> Hsla {
    rgb(0xfbf8f3).into()
}

pub fn border() -> Hsla {
    rgb(0xe3d8cb).into()
}

pub fn heading() -> Hsla {
    rgb(0x3b1418).into()
}

pub fn hover() -> Hsla {
    rgb(0xeadfd0).into()
}

pub fn muted() -> Hsla {
    rgb(0x6e5958).into()
}

pub fn on_accent() -> Hsla {
    rgb(0xf1e4d8).into()
}

pub fn selected() -> Hsla {
    rgb(0x3b1418).into()
}

pub fn text() -> Hsla {
    rgb(0x2b1a1c).into()
}
