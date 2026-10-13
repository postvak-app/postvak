//! The HTML view of a message, without a toolkit: the render thread's protocol, the strips it paints as plain pixels,
//! and which strips to ask for. `state.rs` owns `HtmlState` and talks to the render thread; the window draws it.
#[cfg(feature = "html")]
mod net;
#[cfg(feature = "html")]
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Arc;

use crate::message::{HtmlBody, InlinePart};

use super::body;

/// Strip height in physical pixels.
pub const STRIP: u32 = 512;

/// Layout width in points never goes below this; narrower panels scroll sideways.
pub const MIN_WIDTH: f32 = 320.0;

/// HTML larger than this shows as text.
pub const MAX_HTML: usize = 2 * 1024 * 1024;

/// A laid-out page taller than this, in points, shows as text.
pub const MAX_HEIGHT: f32 = 200_000.0;

/// Seconds the panel width must hold still before the page is laid out again.
pub const SETTLE: f64 = 0.1;

pub const TOO_LARGE: &str = "Too large to render; showing text.";
pub const FAILED: &str = "Could not render HTML; showing text.";

#[cfg(feature = "html")]
pub use render::Renderer;

/// Asks the window to draw again; the render thread calls it after each reply.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A painted strip: `width` × `height` premultiplied RGBA pixels, row by row.
pub struct Pixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

// Without the `html` feature nothing paints strips.
#[cfg_attr(not(feature = "html"), allow(dead_code))]
impl Pixels {
    /// `None` unless `rgba` holds exactly `width` × `height` pixels.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Pixels> {
        let len = u64::from(width)
            .checked_mul(u64::from(height))?
            .checked_mul(4)?;
        let len = usize::try_from(len).ok()?;
        (rgba.len() == len).then_some(Pixels {
            width,
            height,
            rgba,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// Without the `html` feature nothing renders: messages never have an HTML body, so this is never sent to.
#[cfg(not(feature = "html"))]
pub struct Renderer;

#[cfg(not(feature = "html"))]
impl Renderer {
    pub fn start(_wake: Wake) -> Renderer {
        Renderer
    }

    pub fn send(&mut self, _request: Request) {}

    pub fn replies(&self) -> Vec<Reply> {
        Vec::new()
    }
}

/// The HTML body of a raw message when this build renders HTML.
pub fn html_of(raw: &[u8]) -> Option<HtmlBody> {
    if cfg!(feature = "html") {
        crate::message::html_body(raw)
    } else {
        None
    }
}

// Without the `html` feature there is no render thread to read or build these.
#[cfg_attr(not(feature = "html"), allow(dead_code))]
pub struct Load {
    pub generation: u64,
    pub html: String,
    pub inline: Vec<InlinePart>,
    pub scale: f32,
    /// In points.
    pub width: f32,
}

// Without the `html` feature there is no render thread to read or build these.
#[cfg_attr(not(feature = "html"), allow(dead_code))]
pub enum Request {
    Load(Load),
    Paint {
        generation: u64,
        strip: u32,
    },
    /// A point in the page, in points.
    Hit {
        generation: u64,
        x: f32,
        y: f32,
    },
    /// The text between two points in the page, in points; the same point twice clears the selection.
    Select {
        generation: u64,
        from: [f32; 2],
        to: [f32; 2],
    },
}

// Without the `html` feature there is no render thread to read or build these.
#[cfg_attr(not(feature = "html"), allow(dead_code))]
impl Request {
    pub fn generation(&self) -> u64 {
        match self {
            Request::Load(load) => load.generation,
            Request::Paint { generation, .. }
            | Request::Hit { generation, .. }
            | Request::Select { generation, .. } => *generation,
        }
    }
}

// Without the `html` feature there is no render thread to read or build these.
#[cfg_attr(not(feature = "html"), allow(dead_code))]
pub enum Reply {
    Failed {
        generation: u64,
    },
    /// The page size in points; `remote` is set when it asked for remote content.
    Laid {
        generation: u64,
        height: f32,
        remote: bool,
        width: f32,
    },
    Link {
        generation: u64,
        href: Option<String>,
    },
    /// The selected text; the strips painted before it lack its highlight.
    Selected {
        generation: u64,
        text: Option<String>,
    },
    Strip {
        generation: u64,
        image: Pixels,
        strip: u32,
    },
}

impl Reply {
    pub fn generation(&self) -> u64 {
        match self {
            Reply::Failed { generation }
            | Reply::Laid { generation, .. }
            | Reply::Link { generation, .. }
            | Reply::Selected { generation, .. }
            | Reply::Strip { generation, .. } => *generation,
        }
    }
}

/// The HTML view of the shown message: what is laid out, which strips are painted, what the pointer is over.
pub(crate) struct HtmlState {
    /// The layout in flight or shown; replies for any other generation are stale.
    pub generation: u64,
    pub hover: Option<String>,
    /// The last point sent as a hit test, so a still pointer sends nothing.
    pub hovered_at: Option<[f32; 2]>,
    /// Set once the current generation is laid out: the page width and height in points.
    pub page: Option<(f32, f32)>,
    pub remote: bool,
    pub requested: BTreeSet<u32>,
    /// The two points of the last `Select`, so a still drag sends nothing.
    pub selected_at: Option<([f32; 2], [f32; 2])>,
    pub selection: Option<String>,
    /// The scale and width (points) of the last `Load`.
    pub sent: (f32, f32),
    /// The painted strips by number; shared, so the window can show them without copying.
    pub strips: BTreeMap<u32, Arc<Pixels>>,
    /// A different width or scale than `sent`, and when it was first seen.
    pub wanted: Option<((f32, f32), f64)>,
}

impl HtmlState {
    pub fn new(generation: u64, scale: f32, width: f32) -> HtmlState {
        HtmlState {
            generation,
            hover: None,
            hovered_at: None,
            page: None,
            remote: false,
            requested: BTreeSet::new(),
            selected_at: None,
            selection: None,
            sent: (scale, width),
            strips: BTreeMap::new(),
            wanted: None,
        }
    }
}

/// The strips covering `top..bottom` (points) plus one screen either side, within a page `height` points tall.
pub fn wanted_strips(top: f32, bottom: f32, height: f32, scale: f32) -> Range<u32> {
    let screen = (bottom - top).max(1.0);
    let points = STRIP as f32 / scale.max(0.5);
    let first = ((top - screen).max(0.0) / points).floor() as u32;
    let end = ((bottom + screen).min(height) / points).ceil() as u32;
    first..end.max(first)
}

/// Strips more than three screens from `top..bottom` (points), whose textures can go.
pub fn far_strips(
    painted: impl Iterator<Item = u32>,
    top: f32,
    bottom: f32,
    scale: f32,
) -> Vec<u32> {
    let screen = (bottom - top).max(1.0);
    let points = STRIP as f32 / scale.max(0.5);
    painted
        .filter(|&strip| {
            let (start, end) = (strip as f32 * points, (strip + 1) as f32 * points);
            end < top - 3.0 * screen || start > bottom + 3.0 * screen
        })
        .collect()
}

/// A link the HTML view may open: absolute, with one of the text view's schemes.
pub fn openable(href: &str) -> Option<&str> {
    let href = href.trim();
    body::has_target(href).then_some(href)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_hold_exactly_width_times_height() {
        let pixels = Pixels::new(2, 3, vec![0; 24]).unwrap();
        assert_eq!(
            (pixels.width(), pixels.height(), pixels.rgba().len()),
            (2, 3, 24)
        );
        assert!(Pixels::new(2, 3, vec![0; 23]).is_none());
        assert!(Pixels::new(u32::MAX, u32::MAX, Vec::new()).is_none());
    }

    #[test]
    fn wanted_strips_cover_the_view_and_a_screen_either_side() {
        // At scale 1 a strip is 512 points; the view shows 1000..1400 of a 5000-point page.
        assert_eq!(wanted_strips(1000.0, 1400.0, 5000.0, 1.0), 1..4);
        // At the top nothing above, and at scale 2 strips are 256 points.
        assert_eq!(wanted_strips(0.0, 400.0, 5000.0, 2.0), 0..4);
        // A short page stops at its end.
        assert_eq!(wanted_strips(0.0, 800.0, 300.0, 1.0), 0..1);
    }

    #[test]
    fn far_strips_are_three_screens_away() {
        // Screen 400 points at 4000..4400: strips ending before 2800 or starting after 5600 go.
        let far = far_strips(0..12, 4000.0, 4400.0, 1.0);
        assert_eq!(far, [0, 1, 2, 3, 4, 11]);
    }

    #[test]
    fn only_absolute_http_https_and_mailto_links_open() {
        assert_eq!(
            openable(" https://example.com/a "),
            Some("https://example.com/a")
        );
        assert_eq!(
            openable("mailto:me@example.com"),
            Some("mailto:me@example.com")
        );
        for refused in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "/relative",
            "https://",
        ] {
            assert_eq!(openable(refused), None, "{refused}");
        }
    }
}
