//! The HTML view of a message, without a toolkit: the render thread's protocol, the strips it paints as plain pixels,
//! and which strips to ask for. `state.rs` owns `HtmlState` and talks to the render thread; the window draws it.
mod depth;
mod fetch;
mod legacy;
mod net;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Arc;

use postvak_core::message::InlinePart;

use super::body;

/// Strip height in physical pixels.
pub const STRIP: u32 = 512;

/// Layout width in points never goes below this; narrower panels scroll sideways.
pub const MIN_WIDTH: f32 = 320.0;

/// HTML larger than this shows as text.
pub const MAX_HTML: usize = 2 * 1024 * 1024;

/// A laid-out page taller than this, in points, shows as text.
pub const MAX_HEIGHT: f32 = 200_000.0;

/// A laid-out page wider than this, in points, shows as text: every painted strip is this wide.
pub const MAX_WIDTH: f32 = 5_000.0;

/// Seconds the panel width must hold still before the page is laid out again.
pub const SETTLE: f64 = 0.1;

pub const TOO_LARGE: &str = "Too large to render; showing text.";
pub const FAILED: &str = "Could not render HTML; showing text.";

pub(crate) use fetch::{Fetcher, get};
pub use render::Renderer;

/// Asks the window to draw again; the render thread calls it after each reply.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A painted strip: `width` × `height` premultiplied RGBA pixels, row by row.
pub struct Pixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

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

/// Remote content the user chose to load, by URL; a failed download is there with no bytes.
pub(crate) type Fetched = Arc<BTreeMap<String, Vec<u8>>>;

/// A message asking for more remote resources than this gets the first ones only.
pub(crate) const MAX_REMOTE: usize = 200;

/// Remote URLs to download, tagged so the answer for a message no longer shown can be dropped.
pub(crate) struct FetchJob {
    pub tag: u64,
    pub urls: Vec<String>,
}

/// Every URL of a `FetchJob`, with its bytes, or none when the download failed or the host was refused.
pub(crate) struct FetchDone {
    pub tag: u64,
    pub fetched: BTreeMap<String, Vec<u8>>,
}

pub(crate) struct Load {
    pub fetched: Fetched,
    pub generation: u64,
    pub html: String,
    pub inline: Vec<InlinePart>,
    pub scale: f32,
    /// In points.
    pub width: f32,
}

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

pub enum Reply {
    Failed {
        generation: u64,
    },
    /// The page size in points, and the remote URLs it asked for that `Load::fetched` lacks.
    Laid {
        generation: u64,
        height: f32,
        remote: BTreeSet<String>,
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
    /// Remote content downloaded for this message, which every later layout uses.
    pub fetched: Fetched,
    /// The tag of the download in flight.
    pub fetching: Option<u64>,
    /// Set once the current generation is laid out: the page width and height in points.
    pub page: Option<(f32, f32)>,
    /// Remote URLs the last layout asked for and `fetched` lacks.
    pub remote: BTreeSet<String>,
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
    /// The part of the page on screen, in points, as the window last reported it.
    pub visible: (f32, f32),
}

impl HtmlState {
    pub fn new(generation: u64, scale: f32, width: f32) -> HtmlState {
        HtmlState {
            generation,
            hover: None,
            hovered_at: None,
            fetched: Fetched::default(),
            fetching: None,
            page: None,
            remote: BTreeSet::new(),
            requested: BTreeSet::new(),
            selected_at: None,
            selection: None,
            sent: (scale, width),
            strips: BTreeMap::new(),
            wanted: None,
            visible: (0.0, 0.0),
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

#[cfg(test)]
mod view_tests {
    //! The HTML view in the running window: toggling to text, links, notes, remote content, and the render thread's
    //! failures.
    use std::collections::BTreeMap;
    use std::sync::mpsc::{Receiver, Sender};

    use slint::ComponentHandle;

    use super::{
        FAILED, FetchDone, Fetcher, Load, MAX_HEIGHT, MAX_HTML, MAX_WIDTH, Renderer, Reply,
        Request, TOO_LARGE,
    };
    use crate::state::App;
    use crate::test_support::{Fixture, Open, Wires, add_html, launched, message};

    const NEWSLETTER: &str = include_str!("../../../../tests/fixtures/newsletter.html");

    /// Ticks the window until `done` holds; the render thread answers in real time.
    fn wait_for(open: &Open, done: impl Fn(&App) -> bool) {
        open.tick_until(|open| open.with(|live| done(&live.app)));
    }

    /// True once the shown message is laid out and every strip it asked for is painted.
    fn painted(app: &App) -> bool {
        app.body
            .as_ref()
            .and_then(|b| b.html_view.as_ref())
            .is_some_and(|v| {
                v.page.is_some()
                    && !v.requested.is_empty()
                    && v.requested.iter().all(|s| v.strips.contains_key(s))
            })
    }

    fn view<R>(open: &Open, f: impl FnOnce(&super::HtmlState) -> R) -> Option<R> {
        open.with(|live| live.app.body.as_ref()?.html_view.as_ref().map(f))
    }

    /// Moves the pointer onto the page, waits for the link answer, and clicks; the link the page reported.
    fn click_page(open: &Open) -> Option<String> {
        let window = open.window();
        window.invoke_page_hover(200.0, 200.0);
        wait_for(open, |app| {
            app.body
                .as_ref()
                .and_then(|b| b.html_view.as_ref())
                .is_some_and(|v| v.hovered_at.is_some() && v.hover.is_some())
        });
        let hover = view(open, |v| v.hover.clone()).flatten();
        window.invoke_page_clicked(200.0, 200.0);
        hover
    }

    /// A link filling the page, so any point on it is over the link.
    fn page_link(href: &str) -> String {
        format!("<a href=\"{href}\" style=\"display:block;height:3000px\">link</a>")
    }

    #[test]
    fn html_mail_shows_as_html_and_v_switches_that_message_to_text() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>older</p>", "older as text");
        add_html(&fx, 2, NEWSLETTER, "newer as text");
        let (open, _wires) = fx.window();
        wait_for(&open, painted);
        assert!(open.find("Message text").is_empty());
        assert_eq!(open.find("Remote content not loaded.").len(), 1);
        open.press("v");
        assert_eq!(open.window().get_body_text(), "newer as text");
        assert_eq!(open.find("Message text").len(), 1);
        open.press("v");
        assert!(open.find("Message text").is_empty());
        open.press("v j");
        wait_for(&open, painted);
        assert!(open.find("Message text").is_empty());
        assert!(open.find("Remote content not loaded.").is_empty());
    }

    #[test]
    fn v_on_plain_text_mail_changes_nothing() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "plain"));
        let (open, _wires) = fx.window();
        open.press("v");
        assert!(!open.with(|live| live.app.body.as_ref().unwrap().show_text));
        assert_eq!(open.window().get_body_text(), "Body of 1");
        assert!(open.with(|live| live.app.renderer.is_none()));
    }

    #[test]
    fn an_https_link_opens_and_shows_its_target() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, &page_link("https://example.com/x"), "link");
        let (open, _wires) = fx.window();
        wait_for(&open, painted);
        assert_eq!(click_page(&open).as_deref(), Some("https://example.com/x"));
        assert_eq!(open.window().get_page().link, "https://example.com/x");
        assert_eq!(launched(), ["https://example.com/x"]);
    }

    #[test]
    fn a_javascript_link_does_nothing() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, &page_link("javascript:alert(1)"), "link");
        let (open, _wires) = fx.window();
        wait_for(&open, painted);
        assert_eq!(click_page(&open).as_deref(), Some("javascript:alert(1)"));
        assert!(launched().is_empty());
        open.window().invoke_page_open_link();
        assert!(launched().is_empty());
    }

    #[test]
    fn html_over_the_limit_shows_text_with_a_note() {
        let fx = Fixture::new(&["work"]);
        let huge = format!("<p>{}</p>", "x".repeat(MAX_HTML));
        add_html(&fx, 1, &huge, "the text");
        let (open, _wires) = fx.window();
        assert_eq!(open.find(TOO_LARGE).len(), 1);
        assert_eq!(open.window().get_body_text(), "the text");
        assert!(open.with(|live| live.app.renderer.is_none()));
    }

    /// The window with a fake renderer, and the generation of the first layout it asked for.
    struct Faked {
        open: Open,
        jobs: Receiver<Request>,
        replies: Sender<Reply>,
        generation: u64,
        _wires: Wires,
    }

    fn faked(fx: &Fixture) -> Faked {
        let (open, wires) = fx.window();
        let (renderer, jobs, replies) = Renderer::fake(std::sync::Arc::new(|| {}));
        // Opening the window laid out with a real renderer; start the view again on the fake one.
        open.with(|live| {
            live.app.renderer = Some(renderer);
            if let Some(body) = live.app.body.as_mut() {
                body.html_view = None;
            }
        });
        open.tick();
        let generation = loads(&jobs)
            .first()
            .map(|load| load.generation)
            .expect("a layout was asked for");
        Faked {
            open,
            jobs,
            replies,
            generation,
            _wires: wires,
        }
    }

    /// The `Load` jobs the app sent since the last call.
    fn loads(jobs: &Receiver<Request>) -> Vec<Load> {
        jobs.try_iter()
            .filter_map(|job| match job {
                Request::Load(load) => Some(load),
                _ => None,
            })
            .collect()
    }

    fn laid(generation: u64, height: f32, remote: &[&str]) -> Reply {
        Reply::Laid {
            generation,
            height,
            remote: remote.iter().map(|url| url.to_string()).collect(),
            width: 400.0,
        }
    }

    fn reply(faked: &Faked, reply: Reply) {
        faked.replies.send(reply).unwrap();
        faked.open.tick();
    }

    #[test]
    fn a_render_failure_shows_text_with_a_note() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "fallback text");
        let faked = faked(&fx);
        reply(
            &faked,
            Reply::Failed {
                generation: faked.generation,
            },
        );
        assert_eq!(faked.open.find(FAILED).len(), 1);
        assert_eq!(faked.open.window().get_body_text(), "fallback text");
    }

    #[test]
    fn a_reply_for_an_older_layout_is_dropped() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "text");
        let faked = faked(&fx);
        reply(&faked, laid(faked.generation - 1, 900.0, &[]));
        assert_eq!(view(&faked.open, |v| v.page).flatten(), None);
        reply(&faked, laid(faked.generation, 300.0, &[]));
        assert_eq!(
            view(&faked.open, |v| v.page).flatten(),
            Some((400.0, 300.0))
        );
        let page = faked.open.window().get_page();
        assert!(page.ready);
        assert_eq!((page.width, page.height), (400.0, 300.0));
    }

    #[test]
    fn a_new_layout_asks_for_the_strips_on_screen() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "text");
        let faked = faked(&fx);
        reply(&faked, laid(faked.generation, 300.0, &[]));
        let paints: Vec<u32> = faked
            .jobs
            .try_iter()
            .filter_map(|job| match job {
                Request::Paint { strip, .. } => Some(strip),
                _ => None,
            })
            .collect();
        assert_eq!(paints, [0]);
    }

    #[test]
    fn a_page_over_the_height_limit_shows_text_with_a_note() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "tall text");
        let faked = faked(&fx);
        reply(&faked, laid(faked.generation, MAX_HEIGHT + 1.0, &[]));
        assert_eq!(faked.open.find(TOO_LARGE).len(), 1);
        assert_eq!(faked.open.window().get_body_text(), "tall text");
    }

    #[test]
    fn a_page_over_the_width_limit_shows_text_with_a_note() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "wide text");
        let faked = faked(&fx);
        let wide = Reply::Laid {
            generation: faked.generation,
            height: 100.0,
            remote: Default::default(),
            width: MAX_WIDTH + 1.0,
        };
        reply(&faked, wide);
        assert_eq!(faked.open.find(TOO_LARGE).len(), 1);
        assert_eq!(faked.open.window().get_body_text(), "wide text");
    }

    #[test]
    fn a_new_width_lays_out_again_once_it_held_still() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "text");
        let faked = faked(&fx);
        let first = faked.generation;
        faked
            .open
            .window()
            .window()
            .set_size(slint::LogicalSize::new(1180.0, 800.0));
        faked.open.tick();
        assert!(loads(&faked.jobs).is_empty());
        assert!(
            view(&faked.open, |v| v.wanted.is_some()).unwrap(),
            "the new width is waiting to settle"
        );
        faked.open.advance(0.2);
        let generations: Vec<u64> = loads(&faked.jobs)
            .iter()
            .map(|load| load.generation)
            .collect();
        assert_eq!(generations, [first + 1]);
    }

    #[test]
    fn a_drag_selects_page_text_for_copying() {
        let fx = Fixture::new(&["work"]);
        let words = "word ".repeat(2000);
        add_html(
            &fx,
            1,
            &format!("<p style=\"font-size:40px\">{words}</p>"),
            "words",
        );
        let (open, _wires) = fx.window();
        wait_for(&open, painted);
        open.window().invoke_page_select(20.0, 70.0, 220.0, 70.0);
        let selection = |app: &App| {
            app.body
                .as_ref()
                .and_then(|b| b.html_view.as_ref())
                .and_then(|v| v.selection.clone())
        };
        wait_for(&open, |app| selection(app).is_some());
        let selected = open.with(|live| selection(&live.app)).unwrap();
        assert!(selected.contains("word"), "{selected:?}");
        assert_eq!(open.window().get_page_selection(), selected.as_str());
        open.window().invoke_page_clicked(20.0, 70.0);
        wait_for(&open, |app| selection(app).is_none());
        assert_eq!(open.window().get_page_selection(), "");
    }

    #[test]
    fn ctrl_c_copies_the_page_selection() {
        use slint::platform::{Key, WindowEvent};
        let fx = Fixture::new(&["work"]);
        let words = "word ".repeat(200);
        add_html(
            &fx,
            1,
            &format!("<p style=\"font-size:40px\">{words}</p>"),
            "words",
        );
        let (open, _wires) = fx.window();
        wait_for(&open, painted);
        open.window().invoke_page_select(20.0, 70.0, 220.0, 70.0);
        wait_for(&open, |app| {
            let view = app.body.as_ref().and_then(|b| b.html_view.as_ref());
            view.is_some_and(|v| v.selection.is_some())
        });
        let chord = |key: char| {
            let window = open.window();
            let control: slint::SharedString = Key::Control.into();
            for event in [
                WindowEvent::KeyPressed {
                    text: control.clone(),
                },
                WindowEvent::KeyPressed { text: key.into() },
                WindowEvent::KeyReleased { text: key.into() },
                WindowEvent::KeyReleased { text: control },
            ] {
                window.window().dispatch_event(event);
            }
        };
        let selected = open.window().get_page_selection();
        assert!(selected.contains("word"), "{selected:?}");
        chord('c');
        // The search field pastes what Ctrl+C put on the clipboard.
        open.press("/");
        open.tick();
        chord('v');
        open.tick();
        let query = open.with(|live| live.app.search.clone()).unwrap();
        assert_eq!(query, selected.as_str());
    }

    #[test]
    fn loading_remote_content_fetches_what_the_page_asked_for_and_lays_it_out_again() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "text");
        let faked = faked(&fx);
        let (fetcher, fetch_jobs, fetched) = Fetcher::fake();
        faked.open.with(|live| live.app.fetcher = Some(fetcher));
        let hero = "https://cdn.example.com/hero.png";
        reply(&faked, laid(faked.generation, 300.0, &[hero]));
        assert_eq!(faked.open.find("Remote content not loaded.").len(), 1);
        faked.open.click("Load remote content");
        assert_eq!(faked.open.find("Loading remote content…").len(), 1);
        let job = fetch_jobs.try_recv().unwrap();
        assert_eq!(job.urls, [hero]);
        let bytes = BTreeMap::from([(hero.to_string(), b"PNG".to_vec())]);
        fetched
            .send(FetchDone {
                tag: job.tag,
                fetched: bytes,
            })
            .unwrap();
        faked.open.tick();
        let load = loads(&faked.jobs).pop().unwrap();
        assert_eq!(load.fetched[hero], b"PNG");
        reply(&faked, laid(load.generation, 300.0, &[]));
        assert!(faked.open.find("Remote content not loaded.").is_empty());
        assert!(faked.open.find("Loading remote content…").is_empty());
    }

    #[test]
    fn after_loading_what_the_loaded_content_asks_for_follows_without_asking() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>x</p>", "text");
        let faked = faked(&fx);
        let (fetcher, fetch_jobs, fetched) = Fetcher::fake();
        faked.open.with(|live| live.app.fetcher = Some(fetcher));
        let css = "https://cdn.example.com/style.css";
        reply(&faked, laid(faked.generation, 300.0, &[css]));
        faked.open.click("Load remote content");
        let job = fetch_jobs.try_recv().unwrap();
        let bytes = BTreeMap::from([(css.to_string(), b"body{}".to_vec())]);
        fetched
            .send(FetchDone {
                tag: job.tag,
                fetched: bytes,
            })
            .unwrap();
        faked.open.tick();
        let load = loads(&faked.jobs).pop().unwrap();
        let background = "https://cdn.example.com/background.png";
        reply(&faked, laid(load.generation, 300.0, &[background]));
        assert_eq!(fetch_jobs.try_recv().unwrap().urls, [background]);
    }

    #[test]
    fn a_download_for_a_message_no_longer_shown_is_dropped() {
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>one</p>", "one");
        add_html(&fx, 2, "<p>two</p>", "two");
        let faked = faked(&fx);
        let (fetcher, fetch_jobs, fetched) = Fetcher::fake();
        faked.open.with(|live| live.app.fetcher = Some(fetcher));
        reply(
            &faked,
            laid(faked.generation, 300.0, &["https://example.com/a.png"]),
        );
        faked.open.click("Load remote content");
        let job = fetch_jobs.try_recv().unwrap();
        faked.open.press("j");
        let _ = loads(&faked.jobs);
        fetched
            .send(FetchDone {
                tag: job.tag,
                fetched: BTreeMap::new(),
            })
            .unwrap();
        faked.open.tick();
        assert!(loads(&faked.jobs).is_empty());
        assert!(view(&faked.open, |v| v.fetched.is_empty()).unwrap());
    }
}
