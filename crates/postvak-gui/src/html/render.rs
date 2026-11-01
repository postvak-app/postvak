//! The render thread: owns the Blitz document of the shown message, lays it out, paints strips and answers hit tests.
//! Blitz documents are not `Send`, so every one is made, used and dropped here.
use std::collections::{BTreeSet, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{
    BaseDocument, DocumentConfig, FontContext, QualName, UserAgentStylesheet, local_name, ns,
};
use blitz_traits::shell::{ColorScheme, Viewport};
use fontique::{Blob, Collection, CollectionOptions, FallbackKey, Script, SourceCache};
use style::values::computed::Display;

use super::{Load, Pixels, Reply, Request, STRIP, Wake};
use super::{dark, depth, net};

/// Layout, style and paint recurse once per element level, and a stack overflow aborts the app: deeper markup shows as
/// text, and the thread gets a stack that holds this depth.
const MAX_DEPTH: usize = 512;
const STACK: usize = 64 * 1024 * 1024;

/// The narrowest width a page is laid out at. Responsive mail's queries below about 600 px stack its columns with
/// `td { display: block }`, which `block_cells` undoes (#313), and also hide parts and resize text: at 601 px they never
/// match, so such mail keeps its whole desktop layout, and a narrower pane scrolls sideways. Remove both once a pinned
/// Blitz lays out block cells.
const MIN_WIDTH: f32 = 601.0;

/// The thread's name, which the panic hook uses to keep Blitz's panic messages, which can quote the mail, off stderr.
const THREAD: &str = "html-render";

/// A hierarchical base, so relative URLs resolve (Blitz panics on one it cannot resolve) to something the net
/// provider refuses.
const BASE: &str = "postvak://mail/";

/// Mail is written for a 600 px column and assumes nothing about the window: keep images and long words inside it.
/// The rest is the HTML spec's rendering section for the presentational attributes Blitz lacks (`border`, `valign`,
/// `align` on a table or image, `size` on `<font>`); a user-agent sheet, so author CSS wins, as it does over real
/// presentational hints. Relative sizes (`size="+1"`) and `align` on other elements are not mapped.
const MAIL_CSS: &str = concat!(
    "img { max-width: 100%; height: auto; } body { overflow-wrap: anywhere; } ",
    // A border on every cell of a table that asks for one, but not of a table nested in it.
    "table[border]:not([border=\"0\"]) > :is(thead, tbody, tfoot) > tr > :is(td, th), ",
    "table[border]:not([border=\"0\"]) > tr > :is(td, th) { border: 1px solid gray; } ",
    "table[align=\"center\" i] { margin-inline: auto; } ",
    "img[align=\"left\" i] { float: left; } img[align=\"right\" i] { float: right; } ",
    "font[size=\"1\"] { font-size: x-small; } font[size=\"2\"] { font-size: small; } ",
    "font[size=\"3\"] { font-size: medium; } font[size=\"4\"] { font-size: large; } ",
    "font[size=\"5\"] { font-size: x-large; } font[size=\"6\"] { font-size: xx-large; } ",
    "font[size=\"7\"] { font-size: xxx-large; } ",
    ":is(td, th, tr)[valign=\"top\" i] { vertical-align: top; } ",
    ":is(td, th, tr)[valign=\"middle\" i] { vertical-align: middle; } ",
    ":is(td, th, tr)[valign=\"bottom\" i] { vertical-align: bottom; } ",
    ":is(td, th, tr)[valign=\"baseline\" i] { vertical-align: baseline; } ",
    "tr[valign=\"top\" i] > :is(td, th):not([valign]) { vertical-align: top; } ",
    "tr[valign=\"bottom\" i] > :is(td, th):not([valign]) { vertical-align: bottom; } ",
    "tr[valign=\"middle\" i] > :is(td, th):not([valign]) { vertical-align: middle; } ",
    "tr[valign=\"baseline\" i] > :is(td, th):not([valign]) { vertical-align: baseline; }",
);

/// `MAIL_CSS` parsed once, as Blitz parses its default sheet.
static MAIL_SHEET: LazyLock<UserAgentStylesheet> =
    LazyLock::new(|| UserAgentStylesheet::parse(MAIL_CSS));

/// The render thread and its two channels. Dropping it ends the thread once it finishes its current job.
pub struct Renderer {
    wake: Wake,
    replies: Receiver<Reply>,
    requests: Sender<Request>,
}

impl Renderer {
    /// Starts the thread; `wake` runs after each reply, so the window draws it.
    pub fn start(wake: Wake) -> Renderer {
        quiet_panics();
        let (requests, jobs) = mpsc::channel();
        let (done, replies) = mpsc::channel();
        let waker = wake.clone();
        let spawned = std::thread::Builder::new()
            .name(THREAD.into())
            .stack_size(STACK)
            .spawn(move || run(&jobs, &done, &waker, handle));
        if let Err(e) = spawned {
            log::warn!("could not start the HTML renderer: {e}");
        }
        Renderer {
            wake,
            replies,
            requests,
        }
    }

    /// Sends a job; when the thread has gone, starts a new one for a `Load`, as the old document went with it.
    pub fn send(&mut self, request: Request) {
        if let Err(mpsc::SendError(request)) = self.requests.send(request)
            && matches!(request, Request::Load(_))
        {
            *self = Renderer::start(self.wake.clone());
            let _ = self.requests.send(request);
        }
    }

    pub fn replies(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }
}

/// The document being shown, laid out at one width and scale.
struct Current {
    /// Styled with the dark scheme (and adapted, when `originals` holds the styles that replaced).
    dark: bool,
    doc: BaseDocument,
    generation: u64,
    height: u32,
    /// The styles dark adaptation replaced, to put back before the page is styled again.
    originals: dark::Originals,
    raster: VelloCpuImageRenderer,
    /// Every remote URL the page asked for, as the net provider noted them.
    remote: BTreeSet<String>,
    remote_noted: Arc<Mutex<BTreeSet<String>>>,
    scale: f32,
    width: u32,
    /// The narrowest the page is laid out, in points.
    wide: f32,
}

/// Runs jobs until the app hangs up. A panic in `handle` drops the document and replies `Failed`.
fn run(
    jobs: &Receiver<Request>,
    done: &Sender<Reply>,
    wake: &Wake,
    mut handle: impl FnMut(&mut Option<Current>, Request) -> Option<Reply>,
) {
    let mut current: Option<Current> = None;
    while let Ok(first) = jobs.recv() {
        let mut batch = vec![first];
        batch.extend(jobs.try_iter());
        // Only the newest message matters: skip every job before the last `Load`.
        if let Some(last) = batch.iter().rposition(|r| matches!(r, Request::Load(_))) {
            batch.drain(..last);
        }
        // A drag or a moving pointer asks every frame; of each kind of question only the newest is worth answering.
        let mut seen = HashSet::new();
        batch.reverse();
        batch.retain(|request| seen.insert(kind(request)));
        batch.reverse();
        for request in batch {
            if let Some(reply) = answer(&mut current, request, &mut handle) {
                if done.send(reply).is_err() {
                    return;
                }
                wake();
            }
        }
    }
}

/// `handle`'s answer to one request; a panic in it drops the document and answers `Failed`.
fn answer(
    current: &mut Option<Current>,
    request: Request,
    handle: &mut impl FnMut(&mut Option<Current>, Request) -> Option<Reply>,
) -> Option<Reply> {
    let generation = request.generation();
    match panic::catch_unwind(AssertUnwindSafe(|| handle(current, request))) {
        Ok(reply) => reply,
        Err(_) => {
            *current = None;
            // The panic may have left the shared font collection's lock poisoned: scan the fonts again next time.
            FONTS.with_borrow_mut(|fonts| *fonts = None);
            Some(Reply::Failed { generation })
        }
    }
}

/// Requests of the same kind ask the same question; a later one makes an earlier one stale.
fn kind(request: &Request) -> (u8, u32) {
    match request {
        Request::Load(_) => (0, 0),
        Request::Paint { strip, .. } => (1, *strip),
        Request::Hit { .. } => (2, 0),
        Request::Select { .. } => (3, 0),
        Request::Restyle { .. } => (4, 0),
    }
}

fn handle(current: &mut Option<Current>, request: Request) -> Option<Reply> {
    match request {
        Request::Load(load) => {
            let generation = load.generation;
            *current = None;
            let (laid, reply) = layout(load).unzip();
            *current = laid;
            Some(reply.unwrap_or(Reply::Failed { generation }))
        }
        Request::Restyle { generation, dark } => Some(match current.as_mut() {
            Some(current) => restyle(current, generation, dark),
            None => Reply::Failed { generation },
        }),
        Request::Paint { generation, strip } => {
            let current = current.as_mut().filter(|c| c.generation == generation)?;
            Some(Reply::Strip {
                generation,
                strip,
                image: paint(current, strip)?,
            })
        }
        Request::Hit { generation, x, y } => {
            let current = current.as_ref().filter(|c| c.generation == generation)?;
            Some(Reply::Link {
                generation,
                href: link_at(&current.doc, x, y),
            })
        }
        Request::Select {
            generation,
            from,
            to,
        } => {
            let current = current.as_mut().filter(|c| c.generation == generation)?;
            Some(Reply::Selected {
                generation,
                text: select(&mut current.doc, from, to),
            })
        }
    }
}

/// The laid-out document and its reply; None for markup nested too deep to lay out safely.
fn layout(load: Load) -> Option<(Current, Reply)> {
    let scale = load.scale.max(0.5);
    // Never narrower than `MIN_WIDTH`: see there. A narrower pane scrolls sideways.
    let wide = load.width.max(MIN_WIDTH);
    let (mut doc, mut remote) = styled(&load, scale, wide, &[])?;
    let cells = block_cells(&doc);
    // Blitz does not rebuild a table's grid when a restyle changes a cell's display: style a fresh document.
    if !cells.is_empty() {
        (doc, remote) = styled(&load, scale, wide, &cells)?;
    }
    let originals = if load.dark {
        dark::adapt(&mut doc)
    } else {
        Vec::new()
    };
    let mut current = Current {
        dark: load.dark,
        doc,
        generation: load.generation,
        height: 0,
        originals,
        raster: VelloCpuImageRenderer::new(1, 1),
        remote: BTreeSet::new(),
        remote_noted: remote,
        scale,
        width: 0,
        wide,
    };
    let reply = laid(&mut current);
    Some((current, reply))
}

/// `load` parsed and styled at `wide` points, with the `td` and `th` at the positions `cells` turned back into cells
/// (`block_cells`), and what its provider noted of remote content; None for markup nested too deep.
fn styled(
    load: &Load,
    scale: f32,
    wide: f32,
    cells: &[usize],
) -> Option<(BaseDocument, Arc<Mutex<BTreeSet<String>>>)> {
    let (provider, remote) = net::Inline::new(load.inline.clone(), load.fetched.clone());
    let config = DocumentConfig {
        viewport: Some(Viewport::new(
            physical(wide, scale).max(1),
            physical(800.0, scale),
            scale,
            scheme(load.dark),
        )),
        base_url: Some(BASE.into()),
        ua_stylesheets: vec![UserAgentStylesheet::default_sheet(), MAIL_SHEET.clone()],
        net_provider: Some(Arc::new(provider)),
        font_ctx: fonts(),
        ..Default::default()
    };
    let mut doc = BaseDocument::new(config);
    // The parse stops early on most deep markup, but a node it moved can end up deeper than it landed.
    if !depth::parse(&mut doc, &load.html, MAX_DEPTH) || deeper_than(&doc, MAX_DEPTH) {
        return None;
    }
    font_colours(&mut doc);
    table_cells(&mut doc, cells);
    // The provider answered every request synchronously; take its answers before the layout, and the background
    // images the styles asked for after it, so they paint and `dark::adapt` sees which of them loaded.
    doc.handle_messages();
    doc.resolve(0.0);
    doc.handle_messages();
    Some((doc, remote))
}

fn scheme(dark: bool) -> ColorScheme {
    if dark {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    }
}

/// Measures the styled page and answers its layout.
fn laid(current: &mut Current) -> Reply {
    let layout = current.doc.root_element().final_layout();
    let overflow = layout.scrollable_overflow_rect;
    let laid_width = overflow.right.max(current.wide);
    let height = layout.size.height.max(overflow.bottom);
    current.height = physical(height, current.scale);
    current.width =
        physical(laid_width, current.scale).max(physical(current.wide, current.scale).max(1));
    let noted = std::mem::take(
        &mut *current
            .remote_noted
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    current.remote.extend(noted);
    Reply::Laid {
        generation: current.generation,
        height,
        remote: current.remote.clone(),
        width: laid_width,
    }
}

/// Styles the page again with or without the dark scheme. Blitz recascades every element on a colour-scheme change,
/// so the sender's `prefers-color-scheme` rules follow; the HTML is not parsed again.
fn restyle(current: &mut Current, generation: u64, dark: bool) -> Reply {
    if current.dark != dark {
        let originals = std::mem::take(&mut current.originals);
        dark::restore(&mut current.doc, originals);
        let mut viewport = current.doc.get_viewport();
        viewport.color_scheme = scheme(dark);
        current.doc.set_viewport(viewport);
        current.doc.resolve(0.0);
        // A background picture only the new scheme's CSS names was asked for during the cascade: take it in, so it
        // paints and `dark::adapt` sees it loaded, as `layout` does.
        current.doc.handle_messages();
        if dark {
            current.originals = dark::adapt(&mut current.doc);
        }
        current.dark = dark;
    }
    current.generation = generation;
    laid(current)
}

/// Whether any node sits more than `limit` levels below the root; iterative, as recursion is what it guards against.
fn deeper_than(doc: &BaseDocument, limit: usize) -> bool {
    let mut stack = vec![(doc.root_node().id, 0)];
    while let Some((id, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }
        if let Some(node) = doc.get_node(id) {
            stack.extend(node.children.iter().map(|&child| (child, depth + 1)));
        }
    }
    false
}

fn paint(current: &mut Current, strip: u32) -> Option<Pixels> {
    let top = strip.checked_mul(STRIP)?;
    let height = current.height.checked_sub(top)?.min(STRIP);
    if height == 0 {
        return None;
    }
    let (width, scale) = (current.width, f64::from(current.scale));
    current.raster.resize(width, height);
    let mut rgba = Vec::new();
    let doc = &mut current.doc;
    // Blitz's own offsets place the document on the canvas; a strip lower down is the viewport scrolled to it.
    // Hit tests take document coordinates and ignore this scroll.
    doc.set_viewport_scroll(blitz_dom::Point {
        x: 0.0,
        y: f64::from(top) / scale,
    });
    current.raster.render_to_vec(
        |scene| blitz_paint::paint_scene(scene, doc, scale, width, height, 0, 0),
        &mut rgba,
    );
    Pixels::new(width, height, rgba)
}

/// Selects the text from `from` to `to` (CSS pixels) and returns it. A `to` off any text keeps the selection as it
/// was, so a drag past the end of a paragraph does not drop it.
fn select(doc: &mut BaseDocument, from: [f32; 2], to: [f32; 2]) -> Option<String> {
    if from == to {
        doc.clear_text_selection();
        return None;
    }
    if let Some((anchor, anchor_offset)) = doc.find_text_position(from[0], from[1])
        && let Some((focus, focus_offset)) = doc.find_text_position(to[0], to[1])
    {
        doc.set_text_selection(anchor, anchor_offset, focus, focus_offset);
    }
    doc.get_selected_text()
}

/// Blitz maps `bgcolor` but not `<font color>`, which mail still uses for coloured text: copy it into the style.
fn font_colours(doc: &mut BaseDocument) {
    let Ok(fonts) = doc.query_selector_all("font[color]") else {
        return;
    };
    let styles: Vec<_> = fonts
        .into_iter()
        .filter_map(|id| {
            let node = doc.get_node(id)?;
            let colour = node.attr(local_name!("color"))?.trim();
            // Only a colour name or hex value, so the attribute cannot add other declarations.
            let plain = !colour.is_empty()
                && colour
                    .chars()
                    .all(|c| c == '#' || c.is_ascii_alphanumeric());
            let style = node.attr(local_name!("style")).unwrap_or_default();
            plain.then(|| (id, format!("color: {colour}; {style}")))
        })
        .collect();
    let mut mutator = doc.mutate();
    for (id, style) in styles {
        mutator.set_attribute(id, QualName::new(None, ns!(), local_name!("style")), &style);
    }
}

/// Responsive mail stacks its columns with `td { display: block }` at whatever breakpoint it picks, and Blitz lays such
/// a cell out one line high (#313), as it does a floated one: the positions among the page's `td` and `th` of those
/// styled as neither a cell nor hidden, so the sender's `display: none` still hides one.
fn block_cells(doc: &BaseDocument) -> Vec<usize> {
    let Ok(cells) = doc.query_selector_all("td, th") else {
        return Vec::new();
    };
    cells
        .into_iter()
        .enumerate()
        .filter(|&(_, id)| {
            doc.get_node(id)
                .and_then(|node| node.primary_styles())
                .is_some_and(|style| {
                    !matches!(style.clone_display(), Display::TableCell | Display::None)
                })
        })
        .map(|(position, _)| position)
        .collect()
}

/// Makes the `td` and `th` at `positions` cells again, over any author style.
fn table_cells(doc: &mut BaseDocument, positions: &[usize]) {
    let Ok(cells) = doc.query_selector_all("td, th") else {
        return;
    };
    let styles: Vec<_> = positions
        .iter()
        .filter_map(|&position| {
            let id = *cells.get(position)?;
            let style = doc
                .get_node(id)?
                .attr(local_name!("style"))
                .unwrap_or_default();
            // A float would make the cell a block again.
            Some((
                id,
                format!("{style}; display: table-cell !important; float: none !important"),
            ))
        })
        .collect();
    let mut mutator = doc.mutate();
    for (id, style) in styles {
        mutator.set_attribute(id, QualName::new(None, ns!(), local_name!("style")), &style);
    }
}

/// The `href` of the link under a point in CSS pixels, from the innermost `<a>` around the hit node.
fn link_at(doc: &BaseDocument, x: f32, y: f32) -> Option<String> {
    let hit = doc.hit(x, y)?;
    let mut node = doc.get_node(hit.node_id);
    while let Some(n) = node {
        if n.data.is_element_with_tag_name(&local_name!("a")) {
            return n.attr(local_name!("href")).map(str::to_string);
        }
        node = n.parent.and_then(|parent| doc.get_node(parent));
    }
    None
}

/// Points to whole pixels; a float-to-int `as` saturates, and NaN becomes 0.
fn physical(points: f32, scale: f32) -> u32 {
    (points * scale).ceil() as u32
}

thread_local! {
    /// The render thread's fonts, scanned for its first message and cloned for each later one: a clone shares the
    /// scanned system fonts and the font files already read, where a scan takes milliseconds per hundred fonts.
    /// Fonts installed while the thread runs show from the next start.
    static FONTS: std::cell::RefCell<Option<FontContext>> = const { std::cell::RefCell::new(None) };
}

/// The thread's fonts for a new document: system fonts, except in tests, where the bundled Hanken Grotesk alone, so a
/// render looks the same on every machine.
fn fonts() -> Option<FontContext> {
    Some(FONTS.with_borrow_mut(|fonts| fonts.get_or_insert_with(load_fonts).clone()))
}

fn load_fonts() -> FontContext {
    #[cfg(test)]
    {
        let font = include_bytes!("../../assets/fonts/HankenGrotesk-Medium.ttf");
        blitz_dom::build_single_font_ctx(font)
    }
    #[cfg(not(test))]
    system_fonts()
}

/// The system fonts as Blitz would load them, plus symbol fonts for glyphs like `⟶` that the mail's fonts and the
/// script's fallback both lack.
// Tests render with the one bundled font instead.
#[cfg_attr(test, allow(dead_code))]
fn system_fonts() -> FontContext {
    let mut ctx = FontContext {
        source_cache: SourceCache::new_shared(),
        collection: Collection::new(CollectionOptions {
            shared: false,
            system_fonts: true,
        }),
    };
    // Blitz registers this only when it builds the context itself; list markers need it.
    ctx.collection
        .register_fonts(Blob::new(Arc::new(blitz_dom::BULLET_FONT) as _), None);
    append_fallbacks(&mut ctx.collection, SYMBOL_FONTS);
    ctx
}

/// Fonts with arrows, dingbats and maths symbols on macOS and Linux; whichever are installed are used.
#[cfg_attr(test, allow(dead_code))]
const SYMBOL_FONTS: &[&str] = &[
    "Apple Symbols",
    "DejaVu Sans",
    "Noto Sans Math",
    "Noto Sans Symbols 2",
];

/// Adds the installed `families` to the fallbacks every text run consults, whatever its script and language:
/// Parley adds the Han list to each query (linebender/parley#597), so they go after the system's own Han fonts.
fn append_fallbacks(collection: &mut Collection, families: &[&str]) {
    let han = FallbackKey::new(Script::from_bytes(*b"Hani"), None);
    let mut ids: Vec<_> = collection.fallback_families(han).collect();
    ids.extend(
        families
            .iter()
            .filter_map(|name| collection.family_id(name)),
    );
    collection.set_fallbacks(han, ids.into_iter());
}

/// Blitz panics on some input, and its messages can quote the mail; a panic on the render thread logs only that it
/// happened. Other threads keep the previous hook.
fn quiet_panics() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some(THREAD) {
            log::warn!("the HTML renderer failed; showing text instead");
        } else {
            previous(info);
        }
    }));
}

#[cfg(test)]
impl Renderer {
    /// A renderer with no thread: the test reads what the app sent and answers in its place.
    pub fn fake(wake: Wake) -> (Renderer, Receiver<Request>, Sender<Reply>) {
        let (requests, jobs) = mpsc::channel();
        let (done, replies) = mpsc::channel();
        let renderer = Renderer {
            wake,
            replies,
            requests,
        };
        (renderer, jobs, done)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Fetched;
    use super::*;

    /// A pixel of a painted strip, premultiplied RGBA, for asserting on colours.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    struct Colour([u8; 4]);

    impl Colour {
        fn r(self) -> u8 {
            self.0[0]
        }

        fn g(self) -> u8 {
            self.0[1]
        }

        fn b(self) -> u8 {
            self.0[2]
        }

        fn to_array(self) -> [u8; 4] {
            self.0
        }
    }

    /// A strip's size and its pixels as colours, row by row.
    struct Strip {
        size: [usize; 2],
        pixels: Vec<Colour>,
    }

    fn colours(pixels: &Pixels) -> Strip {
        Strip {
            size: [pixels.width() as usize, pixels.height() as usize],
            pixels: pixels
                .rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&p| Colour(p))
                .collect(),
        }
    }

    const NEWSLETTER: &str = include_str!("../../../../tests/fixtures/newsletter.html");

    fn load(generation: u64, html: &str, width: f32) -> Request {
        load_as(generation, html, width, false)
    }

    /// `load`, styled for the dark theme or not.
    fn load_as(generation: u64, html: &str, width: f32, dark: bool) -> Request {
        Request::Load(Load {
            dark,
            fetched: Fetched::default(),
            generation,
            html: html.into(),
            inline: Vec::new(),
            scale: 1.0,
            width,
        })
    }

    /// Answers `requests` one at a time, as the loop answers each, but without merging them into batches: a paint
    /// between two restyles is answered too.
    fn one_by_one(requests: Vec<Request>) -> Vec<Reply> {
        let mut current = None;
        requests
            .into_iter()
            .filter_map(|request| answer(&mut current, request, &mut handle))
            .collect()
    }

    /// Runs `requests` through the loop on this thread and returns the replies.
    fn replies(
        requests: Vec<Request>,
        handler: impl FnMut(&mut Option<Current>, Request) -> Option<Reply>,
    ) -> Vec<Reply> {
        let (send, jobs) = mpsc::channel();
        for request in requests {
            send.send(request).unwrap();
        }
        drop(send);
        let (done, replies) = mpsc::channel();
        let wake: Wake = Arc::new(|| {});
        run(&jobs, &done, &wake, handler);
        replies.try_iter().collect()
    }

    #[test]
    fn a_newsletter_lays_out_paints_and_finds_its_links() {
        let mut current = None;
        let Some(Reply::Laid {
            height,
            remote,
            width,
            ..
        }) = handle(&mut current, load(1, NEWSLETTER, 700.0))
        else {
            panic!("no layout");
        };
        assert!((700.0..800.0).contains(&width), "width {width}");
        assert!(height > 500.0, "height {height}");
        assert_eq!(
            remote.len(),
            2,
            "the tracking pixel and stylesheet were asked for"
        );
        let Some(Reply::Strip { image, .. }) = handle(
            &mut current,
            Request::Paint {
                generation: 1,
                strip: 0,
            },
        ) else {
            panic!("no strip");
        };
        let image = colours(&image);
        assert_eq!(image.size, [700, 512]);
        // The header bar is dark blue, the page around it light grey.
        let at = |x: usize, y: usize| image.pixels[y * 700 + x];
        assert_eq!(at(10, 10).to_array()[..3], [0xf2, 0xf4, 0xf6]);
        // Right of the title, so the bundled font's glyphs never land on it.
        let header = at(600, 40);
        assert!(
            u16::from(header.b()) > u16::from(header.r()) + 20,
            "{header:?}"
        );
        // The second strip holds the quote, the code block and the footer, not blank page.
        let Some(Reply::Strip { image, .. }) = handle(
            &mut current,
            Request::Paint {
                generation: 1,
                strip: 1,
            },
        ) else {
            panic!("no second strip");
        };
        let image = colours(&image);
        let colours: std::collections::HashSet<_> = image.pixels.iter().collect();
        assert!(colours.len() > 20, "{} colours", colours.len());
        // A stale generation gets no answer; a point over the button finds its link.
        let stale = Request::Paint {
            generation: 0,
            strip: 0,
        };
        assert!(handle(&mut current, stale).is_none());
        let mut link = |x: f32, y: f32| match handle(
            &mut current,
            Request::Hit {
                generation: 1,
                x,
                y,
            },
        ) {
            Some(Reply::Link { href, .. }) => href,
            _ => panic!("no hit answer"),
        };
        assert_eq!(link(5.0, 5.0), None);
        let button = (0..height as u32)
            .step_by(4)
            .find_map(|y| link(350.0, y as f32));
        assert_eq!(button.as_deref(), Some("https://example.com/cta"));
    }

    /// Counts the strongly red pixels in the first strip of `html`.
    fn reds(html: &str) -> usize {
        let mut current = None;
        handle(&mut current, load(1, html, 400.0));
        let paint = Request::Paint {
            generation: 1,
            strip: 0,
        };
        let Some(Reply::Strip { image, .. }) = handle(&mut current, paint) else {
            panic!("no strip");
        };
        let image = colours(&image);
        let red = |p: &&Colour| p.r() > 150 && p.g() < 80 && p.b() < 80;
        image.pixels.iter().filter(red).count()
    }

    #[test]
    fn font_colour_attributes_colour_the_text() {
        assert!(reds("<font color=\"#ff0000\">HELLO</font>") > 50);
        assert!(reds("<font color=red style=\"font-weight:bold\">HELLO</font>") > 50);
        // The attribute is a colour, not a place to inject other declarations.
        assert_eq!(reds("<font color=\"x;background:red\">HELLO</font>"), 0);
    }

    #[test]
    fn a_strip_is_plain_premultiplied_rgba() {
        let html = "<p style=\"font-size:40px;margin:0\">HELLO WORLD</p>";
        let mut current = None;
        handle(&mut current, load(1, html, 700.0));
        let paint = Request::Paint {
            generation: 1,
            strip: 0,
        };
        let Some(Reply::Strip { image, .. }) = handle(&mut current, paint) else {
            panic!("no strip");
        };
        assert_eq!(image.width(), 700);
        let len = usize::try_from(image.width() * image.height() * 4).unwrap();
        assert_eq!(image.rgba().len(), len);
        // Premultiplied: no channel exceeds its pixel's alpha.
        assert!(
            image
                .rgba()
                .chunks(4)
                .all(|p| p[..3].iter().all(|&c| c <= p[3]))
        );
        assert!(image.rgba().chunks(4).any(|p| p[3] > 0), "nothing painted");
    }

    #[test]
    fn a_drag_selects_text_and_paints_the_highlight() {
        let html = "<p style=\"font-size:40px;margin:0\">HELLO WORLD</p>";
        let mut current = None;
        handle(&mut current, load(1, html, 600.0));
        let select = |current: &mut Option<Current>, from: (f32, f32), to: (f32, f32)| match handle(
            current,
            Request::Select {
                generation: 1,
                from: [from.0, from.1],
                to: [to.0, to.1],
            },
        ) {
            Some(Reply::Selected { text, .. }) => text,
            _ => panic!("no selection answer"),
        };
        assert_eq!(
            select(&mut current, (10.0, 20.0), (590.0, 20.0)).as_deref(),
            Some("HELLO WORLD")
        );
        // A drag out past the text keeps what it had; a click clears it.
        assert_eq!(
            select(&mut current, (10.0, 20.0), (590.0, 700.0)).as_deref(),
            Some("HELLO WORLD")
        );
        assert_eq!(select(&mut current, (10.0, 20.0), (10.0, 20.0)), None);
        let paint = Request::Paint {
            generation: 1,
            strip: 0,
        };
        let plain = match handle(&mut current, paint) {
            Some(Reply::Strip { image, .. }) => colours(&image),
            _ => panic!("no strip"),
        };
        let _ = select(&mut current, (10.0, 20.0), (590.0, 20.0));
        let paint = Request::Paint {
            generation: 1,
            strip: 0,
        };
        let Some(Reply::Strip { image, .. }) = handle(&mut current, paint) else {
            panic!("no strip");
        };
        let image = colours(&image);
        assert_ne!(image.pixels, plain.pixels, "the selection is not painted");
    }

    #[test]
    fn a_batch_keeps_only_the_newest_selection_paint_and_hit() {
        let select = |x| Request::Select {
            generation: 1,
            from: [0.0, 0.0],
            to: [x, 0.0],
        };
        let paint = |strip| Request::Paint {
            generation: 1,
            strip,
        };
        let requests = vec![
            load(1, "<p>x</p>", 400.0),
            select(1.0),
            paint(0),
            select(2.0),
            paint(0),
        ];
        let mut seen = Vec::new();
        replies(requests, |_, request| {
            seen.push(match request {
                Request::Load(_) => "load".to_string(),
                Request::Select { to, .. } => format!("select {}", to[0]),
                Request::Paint { strip, .. } => format!("paint {strip}"),
                Request::Hit { .. } => "hit".to_string(),
                Request::Restyle { .. } => "restyle".to_string(),
            });
            None
        });
        assert_eq!(seen, ["load", "select 2", "paint 0"]);
    }

    #[test]
    fn a_strip_past_the_end_is_not_painted() {
        let mut current = None;
        handle(&mut current, load(1, "<p>short</p>", 400.0));
        let past = Request::Paint {
            generation: 1,
            strip: 3,
        };
        assert!(handle(&mut current, past).is_none());
    }

    #[test]
    fn relative_urls_and_a_bare_page_do_not_panic() {
        let html = "<img src=\"logo.png\"><a href=\"/x\">x</a><link rel=stylesheet href=\"s.css\"><iframe src=\"f.html\"></iframe>";
        let got = replies(vec![load(1, html, 400.0), load(2, "", 400.0)], handle);
        assert!(matches!(got[..], [Reply::Laid { generation: 2, .. }]));
    }

    #[test]
    fn mail_declaring_the_xhtml_namespace_is_parsed_as_html() {
        // Parsed as XML, the void `<img>` would swallow the table, and its `display: none` the whole page.
        let html = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><img src="cid:x" style="display:none"><table><tr><td style="height:300px">Hi</td></tr></table></body></html>"#;
        let mut current = None;
        let Some(Reply::Laid { height, .. }) = handle(&mut current, load(1, html, 400.0)) else {
            panic!("no layout");
        };
        assert!(height >= 300.0, "height {height}");
    }

    #[test]
    fn installed_symbol_fonts_join_the_fallbacks_of_every_script() {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let data = include_bytes!("../../assets/fonts/HankenGrotesk-Medium.ttf");
        let registered = collection.register_fonts(Blob::new(Arc::new(data.to_vec()) as _), None);
        let id = registered[0].0;
        let name = collection.family_name(id).unwrap().to_string();
        append_fallbacks(&mut collection, &["Not Installed", &name]);
        let han = FallbackKey::new(Script::from_bytes(*b"Hani"), None);
        assert_eq!(collection.fallback_families(han).collect::<Vec<_>>(), [id]);
    }

    #[test]
    fn queued_loads_lay_out_only_the_newest() {
        let requests = vec![
            load(1, "<p>one</p>", 400.0),
            Request::Paint {
                generation: 1,
                strip: 0,
            },
            load(2, "<p>two</p>", 400.0),
            Request::Paint {
                generation: 2,
                strip: 0,
            },
        ];
        let got: Vec<_> = replies(requests, handle)
            .iter()
            .map(|r| match r {
                Reply::Laid { generation, .. } => ("laid", *generation),
                Reply::Strip { generation, .. } => ("strip", *generation),
                _ => ("other", 0),
            })
            .collect();
        assert_eq!(got, [("laid", 2), ("strip", 2)]);
    }

    fn nested(depth: usize) -> String {
        format!("{}x{}", "<div>".repeat(depth), "</div>".repeat(depth))
    }

    #[test]
    fn markup_nested_too_deep_replies_failed() {
        // Just past the limit: html5ever parses nesting in quadratic time, so a deeper page only slows the test.
        for dark in [false, true] {
            let got = replies(
                vec![load_as(3, &nested(MAX_DEPTH + 8), 400.0, dark)],
                handle,
            );
            assert!(
                matches!(got[..], [Reply::Failed { generation: 3 }]),
                "{dark}"
            );
        }
    }

    #[test]
    fn markup_just_under_the_depth_limit_lays_out_and_paints_on_the_render_stack() {
        let html = nested(MAX_DEPTH - 8);
        let painted = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(move || {
                let paint = |generation| Request::Paint {
                    generation,
                    strip: 0,
                };
                // Light and dark, then switched to the other scheme: adapting walks the same depth.
                [false, true].map(|dark| {
                    let mut current = None;
                    let laid = handle(&mut current, load_as(1, &html, 400.0, dark));
                    let hit = Request::Hit {
                        generation: 1,
                        x: 5.0,
                        y: 5.0,
                    };
                    handle(&mut current, hit);
                    let restyle = Request::Restyle {
                        generation: 2,
                        dark: !dark,
                    };
                    matches!(laid, Some(Reply::Laid { .. }))
                        && matches!(handle(&mut current, paint(1)), Some(Reply::Strip { .. }))
                        && matches!(handle(&mut current, restyle), Some(Reply::Laid { .. }))
                        && matches!(handle(&mut current, paint(2)), Some(Reply::Strip { .. }))
                })
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(painted, [true; 2]);
    }

    #[test]
    fn a_cell_a_stacking_media_query_makes_a_block_keeps_its_content_and_a_hidden_cell_stays_hidden()
     {
        let paragraphs = "<p>a</p><p>b</p><p>c</p><p>d</p><p>e</p>";
        let page = |style: &str| {
            format!(
                "<style>@media (max-width:620px){{{style}}}</style><table width=\"600\"><tr>\
                 <td class=\"column\">{paragraphs}</td><td class=\"hidden\" style=\"display:none\">x</td></tr></table>"
            )
        };
        let height = |html: &str| laid_out(html, false, Vec::new()).height;
        let plain = height(&page(""));
        assert!(plain > 100, "{plain}");
        for stack in [
            ".column{display:block}",
            ".column{display:inline-block!important}",
        ] {
            assert_eq!(height(&page(stack)), plain, "{stack}");
        }
        let current = laid_out(&page(".column{display:block}"), false, Vec::new());
        let Some(hidden) = current
            .doc
            .query_selector(".hidden")
            .ok()
            .flatten()
            .and_then(|id| current.doc.get_node(id))
            .and_then(|node| node.primary_styles())
        else {
            panic!("no hidden cell");
        };
        assert_eq!(hidden.clone_display(), Display::None);
    }

    #[test]
    fn a_panic_drops_the_document_and_replies_failed() {
        let got = replies(vec![load(7, "<p>x</p>", 400.0)], |_, _| {
            panic!("blitz fell over")
        });
        assert!(matches!(got[..], [Reply::Failed { generation: 7 }]));
    }

    #[test]
    fn the_fonts_load_once_per_thread_and_a_mails_own_stay_with_it() {
        assert!(FONTS.with_borrow(Option::is_none));
        let Some(mut first) = fonts() else {
            panic!("no fonts");
        };
        assert!(FONTS.with_borrow(Option::is_some));
        // What a mail's `@font-face` registers goes into its document's copy, not the cached one.
        let font = include_bytes!("../../assets/fonts/BricolageGrotesque-SemiBold.ttf");
        first
            .collection
            .register_fonts(Blob::new(Arc::new(font.as_slice()) as _), None);
        assert!(first.collection.family_id("Bricolage Grotesque").is_some());
        let Some(mut next) = fonts() else {
            panic!("no fonts");
        };
        assert_eq!(next.collection.family_id("Bricolage Grotesque"), None);
        assert!(next.collection.family_id("Hanken Grotesk").is_some());
    }

    #[test]
    fn a_panic_drops_the_cached_fonts() {
        let mut current = None;
        answer(&mut current, load(1, "<p>x</p>", 400.0), &mut handle);
        assert!(FONTS.with_borrow(Option::is_some));
        answer(&mut current, load(2, "<p>x</p>", 400.0), &mut |_, _| {
            panic!("blitz fell over")
        });
        assert!(FONTS.with_borrow(Option::is_none));
    }

    /// The first strip of `html` laid out at `width`, as colours.
    fn strip(html: &str, width: f32) -> Strip {
        let mut current = None;
        handle(&mut current, load(1, html, width));
        let Some(Reply::Strip { image, .. }) = handle(
            &mut current,
            Request::Paint {
                generation: 1,
                strip: 0,
            },
        ) else {
            panic!("no strip");
        };
        colours(&image)
    }

    /// Where, as (x, y), the pixels of `strip` that `test` accepts are.
    fn at(strip: &Strip, test: impl Fn(Colour) -> bool) -> Vec<(usize, usize)> {
        let width = strip.size[0];
        (0..strip.pixels.len())
            .filter(|&i| test(strip.pixels[i]))
            .map(|i| (i % width, i / width))
            .collect()
    }

    /// Painted, whatever the colour: the strip is transparent where nothing is.
    fn is_ink(p: Colour) -> bool {
        p.0[3] > 0
    }

    fn is_red(p: Colour) -> bool {
        p.r() > 200 && p.g() < 60 && p.b() < 60
    }

    /// A 100 x 100 cell with a legacy `bgcolor`, and the colour it paints at its middle.
    fn cell(bgcolor: &str) -> [u8; 3] {
        let html = format!(
            "<body bgcolor=\"#ffffff\"><table cellpadding=0 cellspacing=0><tr><td width=100 height=100 bgcolor=\"{bgcolor}\"></td></tr></table></body>"
        );
        let image = strip(&html, 400.0);
        let p = image.pixels[58 * image.size[0] + 58];
        [p.r(), p.g(), p.b()]
    }

    #[test]
    fn a_legacy_background_colour_is_read_as_the_spec_says() {
        // Three digits are doubled; Blitz alone reads "#ddd" as 0x0d0d0d, near black.
        assert_eq!(cell("#ddd"), [0xdd; 3]);
        assert_eq!(cell("red"), [255, 0, 0]);
        assert_eq!(cell("RED"), [255, 0, 0]);
        assert_eq!(cell("ff0000"), [255, 0, 0]);
        assert_eq!(cell(" #00f "), [0, 0, 255]);
        assert_eq!(cell("#123456"), [0x12, 0x34, 0x56]);
        // Junk is read leniently, never as an error that drops the colour.
        assert_ne!(cell("chucknorris"), [255; 3]);
    }

    #[test]
    fn a_table_border_attribute_draws_cell_borders() {
        let table = |border: &str| {
            format!(
                "<table {border} cellpadding=0 cellspacing=0><tr><td width=50 height=50></td></tr></table>"
            )
        };
        let inked = |html: &str| at(&strip(html, 400.0), is_ink).len();
        assert!(inked(&table("border=1")) > 100);
        assert_eq!(inked(&table("border=0")), 0);
        assert_eq!(inked(&table("")), 0);
    }

    #[test]
    fn a_cell_valign_places_its_content() {
        let cell = |valign: &str| {
            let html = format!(
                "<table cellpadding=0 cellspacing=0><tr><td height=100 {valign}><div style=\"width:10px;height:10px;background:#f00\"></div></td></tr></table>"
            );
            let reds = at(&strip(&html, 400.0), is_red);
            assert!(!reds.is_empty(), "nothing painted for {valign}");
            reds.iter().map(|p| p.1).max().unwrap_or(0)
        };
        assert!(cell("valign=bottom") > 80);
        assert!(cell("valign=top") < 30);
    }

    #[test]
    fn a_table_with_align_center_is_centred() {
        let html = "<table align=center width=100 cellpadding=0><tr><td height=20 style=\"background:#f00\"></td></tr></table>";
        let reds = at(&strip(html, 700.0), is_red);
        let left = reds.iter().map(|p| p.0).min().unwrap_or(0);
        assert!((290..310).contains(&left), "left edge {left}");
    }

    #[test]
    fn an_image_with_align_floats() {
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let image = |align: &str| {
            let html = format!(
                "<img {align} width=40 height=40 src=\"{png}\"><p>some text beside the picture</p>"
            );
            let reds = at(&strip(&html, 400.0), is_red);
            assert!(!reds.is_empty(), "image not painted for {align}");
            reds.iter().map(|p| p.0).min().unwrap_or(0)
        };
        assert!(image("align=right") > 300);
        assert!(image("align=left") < 20);
    }

    #[test]
    fn a_font_size_attribute_sizes_the_text() {
        let size = |attrs: &str| {
            at(
                &strip(
                    &format!("<font {attrs} color=\"#ff0000\">HELLO</font>"),
                    400.0,
                ),
                is_red,
            )
            .len()
        };
        let normal = size("");
        assert!(size("size=5") > normal * 3 / 2);
        assert!(size("size=2") < normal);
    }

    #[test]
    fn legacy_attributes_give_way_to_author_css() {
        let html = "<table border=1 cellpadding=0 cellspacing=0><tr><td width=50 height=50 valign=bottom style=\"border:0\"></td></tr></table><style>td { vertical-align: top }</style>";
        assert!(at(&strip(html, 400.0), is_ink).is_empty());
        let img = "<table cellspacing=0 cellpadding=0><tr><td height=100 valign=bottom style=\"vertical-align:top\"><div style=\"width:10px;height:10px;background:#f00\"></div></td></tr></table>";
        let reds = at(&strip(img, 400.0), is_red);
        assert!(reds.iter().all(|p| p.1 < 30));
        assert!(!reds.is_empty());
    }

    /// The stacking rule responsive mail ships for narrow screens, on two columns with a colour and a text each.
    const STACKING: &str = "<style>@media (max-width: 600px) { td.col { display: block !important; width: 100% !important } }</style><table width=100% cellpadding=0 cellspacing=0><tr><td class=col style=\"background:#ff0000;color:#00ff00\">one</td><td class=col style=\"background:#0000ff;color:#ffff00\">two</td></tr></table>";

    /// Below 600 px Blitz paints nothing of a cell the mail's media query makes a block, so the page is laid out at
    /// 601 px or more, where the query never matches, and the pane scrolls sideways.
    #[test]
    fn responsive_mail_in_a_narrow_pane_keeps_both_columns() {
        let mut current = None;
        let Some(Reply::Laid { width, .. }) = handle(&mut current, load(1, STACKING, 400.0)) else {
            panic!("no layout");
        };
        assert!(width >= 601.0, "laid out {width} wide");
        let Some(Reply::Strip { image, .. }) = handle(
            &mut current,
            Request::Paint {
                generation: 1,
                strip: 0,
            },
        ) else {
            panic!("no strip");
        };
        let image = colours(&image);
        let has = |r, g, b| {
            image
                .pixels
                .iter()
                .any(|p| [p.r(), p.g(), p.b()] == [r, g, b])
        };
        assert!(has(255, 0, 0), "first column background");
        assert!(has(0, 0, 255), "second column background");
        // Text is anti-aliased, so look for strongly coloured pixels rather than the exact colour.
        let count = |keep: fn(&Colour) -> bool| image.pixels.iter().filter(|p| keep(p)).count();
        assert!(
            count(|p| p.g() > 200 && p.r() < 60 && p.b() < 60) > 10,
            "first column text"
        );
        assert!(
            count(|p| p.r() > 200 && p.g() > 200 && p.b() < 60) > 10,
            "second column text"
        );
    }

    #[test]
    fn a_pane_wider_than_the_minimum_keeps_its_own_width() {
        let mut current = None;
        let Some(Reply::Laid { width, .. }) = handle(&mut current, load(1, "<p>hi</p>", 900.0))
        else {
            panic!("no layout");
        };
        assert!((900.0..901.0).contains(&width), "laid out {width} wide");
    }

    /// `html` laid out at 400 points, light or styled for the dark theme, with `inline` as its `cid:` parts.
    fn laid_out(html: &str, dark: bool, inline: Vec<postvak_core::message::InlinePart>) -> Current {
        let load = Load {
            dark,
            fetched: Fetched::default(),
            generation: 1,
            html: html.into(),
            inline,
            scale: 1.0,
            width: 400.0,
        };
        let Some((current, _)) = layout(load) else {
            panic!("not laid out");
        };
        current
    }

    fn first_strip(current: &mut Current) -> Strip {
        let Some(pixels) = paint(current, 0) else {
            panic!("no strip");
        };
        colours(&pixels)
    }

    /// The computed colour of the first element `selector` matches.
    fn colour_of(current: &Current, selector: &str) -> [f32; 3] {
        let doc = &current.doc;
        let Some(node) = doc
            .query_selector(selector)
            .ok()
            .flatten()
            .and_then(|id| doc.get_node(id))
        else {
            panic!("no {selector}");
        };
        let Some(style) = node.primary_styles() else {
            panic!("{selector} has no style");
        };
        let [r, g, b, _] = style
            .clone_color()
            .to_color_space(style::color::ColorSpace::Srgb)
            .raw_components()
            .to_owned();
        [r, g, b]
    }

    fn pixel(strip: &Strip, x: usize, y: usize) -> [u8; 3] {
        let p = strip.pixels[y * strip.size[0] + x];
        [p.r(), p.g(), p.b()]
    }

    fn rgb(p: [u8; 3]) -> [f32; 3] {
        p.map(|v| f32::from(v) / 255.0)
    }

    const CARD: &str = "<body bgcolor=\"#ffffff\"><table cellpadding=0 cellspacing=0><tr><td width=100 height=100 bgcolor=\"#fde8e8\"></td></tr></table><p style=\"color: #333333 !important\">Read me</p><p><font color=\"#1e3a8a\">Blue</font></p></body>";

    #[test]
    fn a_bgcolor_cell_and_important_text_are_adapted_in_the_dark_theme() {
        let mut light = laid_out(CARD, false, Vec::new());
        let mut dark = laid_out(CARD, true, Vec::new());
        assert!(light.originals.is_empty());
        assert!(!dark.originals.is_empty());
        let (light_cell, dark_cell) = (
            pixel(&first_strip(&mut light), 58, 58),
            pixel(&first_strip(&mut dark), 58, 58),
        );
        assert_eq!(light_cell, [0xfd, 0xe8, 0xe8]);
        // The pink cell turns dark and stays reddish.
        assert!(
            crate::theme::luminance(rgb(dark_cell)) < 0.05,
            "{dark_cell:?}"
        );
        assert!(
            dark_cell[0] > dark_cell[1] && dark_cell[0] > dark_cell[2],
            "{dark_cell:?}"
        );
        let page = pixel(&first_strip(&mut dark), 300, 2);
        for (ours, theirs) in rgb(page).iter().zip(dark::PAGE) {
            assert!((ours - theirs).abs() < 0.01, "{page:?}");
        }
        for selector in ["p", "font"] {
            let text = colour_of(&dark, selector);
            assert!(
                crate::theme::contrast(text, dark::PAGE) >= 4.5,
                "{selector}: {text:?}"
            );
        }
        assert_eq!(colour_of(&light, "p"), rgb([0x33; 3]));
    }

    /// A 20 × 20 PNG, white on the left half and blue on the right.
    fn half_white() -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 20, 20);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        let row: Vec<u8> = (0..20)
            .flat_map(|x| {
                if x < 10 {
                    [255, 255, 255]
                } else {
                    [20, 60, 200]
                }
            })
            .collect();
        writer.write_image_data(&row.repeat(20)).unwrap();
        writer.finish().unwrap();
        out
    }

    /// A `size` × `size` PNG of one colour.
    fn solid(size: u32, colour: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, size, size);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&colour.repeat((size * size) as usize))
            .unwrap();
        writer.finish().unwrap();
        out
    }

    fn part(cid: &str, bytes: Vec<u8>) -> postvak_core::message::InlinePart {
        postvak_core::message::InlinePart {
            cid: cid.into(),
            bytes,
        }
    }

    /// The computed background colour of the first element `selector` matches.
    fn background_of(current: &Current, selector: &str) -> [f32; 4] {
        let doc = &current.doc;
        let Some(style) = doc
            .query_selector(selector)
            .ok()
            .flatten()
            .and_then(|id| doc.get_node(id))
            .and_then(|node| node.primary_styles())
        else {
            panic!("no style for {selector}");
        };
        let current_colour = style.clone_color();
        let [r, g, b, a] = style
            .get_background()
            .background_color
            .resolve_to_absolute(&current_colour)
            .to_color_space(style::color::ColorSpace::Srgb)
            .raw_components()
            .to_owned();
        [r, g, b, a]
    }

    fn reads_on_the_page(current: &Current, selector: &str) -> bool {
        crate::theme::contrast(colour_of(current, selector), dark::PAGE) >= 4.5
    }

    #[test]
    fn a_dark_header_above_a_white_article_leaves_the_page_light_and_adapted() {
        let html = "<body><table width=100% bgcolor=#111><tr><td style=color:#fff>Brand</td></tr></table><table width=100% bgcolor=#fff><tr><td>Article</td></tr></table><p>Unsubscribe · 1 Main St</p></body>";
        let dark = laid_out(html, true, Vec::new());
        assert!(!dark.originals.is_empty(), "adapted");
        // The article turns dark, its text and the footer read on the dark page, and the header stays as it was.
        let article = background_of(&dark, "table + table");
        assert!(
            crate::theme::luminance([article[0], article[1], article[2]]) < 0.05,
            "{article:?}"
        );
        assert!(reads_on_the_page(&dark, "table + table td"));
        assert!(reads_on_the_page(&dark, "p"));
        assert_eq!(colour_of(&dark, "td"), [1.0; 3]);
        assert_eq!(background_of(&dark, "table")[..3], rgb([0x11; 3]));
    }

    #[test]
    fn a_page_mostly_in_dark_containers_keeps_them_but_its_bare_text_reads() {
        let html = "<body><table width=100% height=300 bgcolor=#111><tr><td style=color:#fff>Brand</td></tr></table><table width=100% bgcolor=#fff><tr><td>Article</td></tr></table><p>Unsubscribe · 1 Main St</p></body>";
        let dark = laid_out(html, true, Vec::new());
        // The sender's surfaces and the text on them stay; the footer on no surface of theirs is on the dark pane.
        assert_eq!(background_of(&dark, "table + table"), [1.0; 4]);
        assert_eq!(colour_of(&dark, "table + table td"), [0.0; 3]);
        assert_eq!(colour_of(&dark, "td"), [1.0; 3]);
        assert!(reads_on_the_page(&dark, "p"));
    }

    #[test]
    fn text_beside_a_small_background_icon_is_adapted() {
        let html = "<ul><li style=\"background:url(cid:check) no-repeat 0 4px;padding-left:20px;color:#333\">Done</li></ul>";
        let dark = laid_out(html, true, vec![part("check", solid(12, [0, 160, 0]))]);
        assert!(reads_on_the_page(&dark, "li"));
        // The same icon sized to fill the box is what the text is on: it keeps the sender's colour.
        let html = "<ul><li style=\"background:url(cid:check) no-repeat;background-size:100% 100%;color:#333\">Done</li></ul>";
        let dark = laid_out(html, true, vec![part("check", solid(12, [250, 250, 250]))]);
        assert_eq!(colour_of(&dark, "li"), rgb([0x33; 3]));
    }

    #[test]
    fn text_on_a_hero_picture_that_fills_less_than_half_its_cell_keeps_its_colour() {
        // The picture spans the cell's width but paints only 44% of it: the headline is on it, not beside it.
        let html = "<table width=400 cellspacing=0 cellpadding=0><tr><td style=\"background:url(cid:hero) no-repeat top center;color:#333;height:900px;vertical-align:top\"><h1>Headline</h1><p>Body copy</p></td></tr></table>";
        let dark = laid_out(html, true, vec![part("hero", solid(400, [240, 240, 240]))]);
        assert_eq!(colour_of(&dark, "h1"), rgb([0x33; 3]));
    }

    #[test]
    fn image_pixels_are_the_same_adapted_or_not() {
        let html = "<body style=\"margin:0;background:#ffffff\"><img src=\"cid:logo\" width=20 height=20 style=\"display:block\"></body>";
        let part = || {
            vec![postvak_core::message::InlinePart {
                cid: "logo".into(),
                bytes: half_white(),
            }]
        };
        let light = first_strip(&mut laid_out(html, false, part()));
        let dark = first_strip(&mut laid_out(html, true, part()));
        assert_eq!(pixel(&light, 300, 10), [255; 3]);
        assert_ne!(pixel(&dark, 300, 10), [255; 3], "the page is adapted");
        for y in 0..20 {
            for x in 0..20 {
                assert_eq!(pixel(&light, x, y), pixel(&dark, x, y), "({x}, {y})");
            }
        }
        assert_eq!(pixel(&dark, 2, 2), [255; 3]);
        assert_eq!(pixel(&dark, 15, 2), [20, 60, 200]);
    }

    #[test]
    fn a_gradient_turns_dark_and_a_background_picture_keeps_its_colours() {
        let html = "<body style=\"margin:0\"><div style=\"height:100px;background:linear-gradient(#e8f0fe,#e8f0fe)\"></div><div style=\"background:#ffffff url(cid:logo);color:#333333\">On a picture</div></body>";
        let part = vec![postvak_core::message::InlinePart {
            cid: "logo".into(),
            bytes: half_white(),
        }];
        let mut dark = laid_out(html, true, part);
        let band = pixel(&first_strip(&mut dark), 300, 50);
        assert!(crate::theme::luminance(rgb(band)) < 0.05, "{band:?}");
        assert!(band[2] > band[0], "the blue stays blue: {band:?}");
        assert_eq!(colour_of(&dark, "div + div"), rgb([0x33; 3]));
        // The picture itself is painted: white on the left, blue on the right.
        let strip = first_strip(&mut dark);
        assert_eq!(pixel(&strip, 2, 102), [255; 3]);
        assert_eq!(pixel(&strip, 15, 102), [20, 60, 200]);
    }

    #[test]
    fn light_text_from_the_senders_dark_css_on_no_background_stays_visible() {
        // The sender relies on the reader's dark canvas and paints no background of their own.
        let html = "<meta name=\"color-scheme\" content=\"light dark\"><style>@media (prefers-color-scheme: dark) { .text { color: #f1f1f1 !important } }</style><table><tr><td class=text>Hello</td></tr></table>";
        let dark = laid_out(html, true, Vec::new());
        assert_eq!(colour_of(&dark, "td"), rgb([0xf1; 3]));
    }

    #[test]
    fn text_hidden_on_the_senders_own_background_stays_hidden() {
        let html = "<body style=\"background:#ffffff\"><div style=\"color:#fefefe\">Preheader</div><p>Body</p></body>";
        let dark = laid_out(html, true, Vec::new());
        let hidden = colour_of(&dark, "div");
        assert!(
            crate::theme::contrast(hidden, dark::PAGE) < 1.2,
            "{hidden:?}"
        );
        assert!(crate::theme::contrast(colour_of(&dark, "p"), dark::PAGE) >= 4.5);
    }

    #[test]
    fn text_on_a_background_picture_that_did_not_load_is_adapted() {
        // Remote content is not shown, so the hero photo the dark heading was written for never paints.
        let html = "<table><tr><td style=\"background-image:url(https://cdn.example/hero.jpg)\"><h1 style=\"color:#222222\">Spring sale</h1></td></tr></table>";
        let dark = laid_out(html, true, Vec::new());
        let heading = colour_of(&dark, "h1");
        assert!(
            crate::theme::contrast(heading, dark::PAGE) >= 4.5,
            "{heading:?}"
        );
    }

    #[test]
    fn a_page_with_its_own_dark_css_is_left_alone() {
        let html = "<style>@media (prefers-color-scheme: dark) { body { background: #101820; color: #dddddd } }</style><p>Night</p>";
        let mut dark = laid_out(html, true, Vec::new());
        assert!(dark.originals.is_empty());
        assert_eq!(pixel(&first_strip(&mut dark), 300, 2), [0x10, 0x18, 0x20]);
        assert_eq!(colour_of(&dark, "p"), rgb([0xdd; 3]));
        // In the light theme the same mail is in its light colours.
        let light = laid_out(html, false, Vec::new());
        assert_eq!(colour_of(&light, "p"), [0.0; 3]);
    }

    #[test]
    fn a_dark_outer_table_counts_as_a_dark_page() {
        let html = "<table width=\"100%\" bgcolor=\"#1b1b1b\"><tr><td style=\"color:#eeeeee\">Dark design<div style=\"background:#ffffff;color:#111111\">Coupon</div></td></tr></table>";
        let dark = laid_out(html, true, Vec::new());
        assert_eq!(background_of(&dark, "table")[..3], rgb([0x1b; 3]));
        assert_eq!(colour_of(&dark, "td"), rgb([0xee; 3]));
        // A page judged light would turn the white box inside dark; a dark page keeps the sender's surfaces.
        assert_eq!(background_of(&dark, "div"), [1.0; 4]);
        assert_eq!(colour_of(&dark, "div"), rgb([0x11; 3]));
    }

    /// A logo each scheme's CSS names a picture of its own for, with dark text on it, above a light article.
    const SCHEMED: &str = "<style>.logo { height: 40px; color: #222222; background-image: url(cid:logo-light) } @media (prefers-color-scheme: dark) { .logo { background-image: url(cid:logo-dark) } }</style><body style=\"margin:0;background:#ffffff\"><div class=logo>Brand</div><p style=\"color:#333333\">Article</p></body>";

    fn logos() -> Vec<postvak_core::message::InlinePart> {
        vec![
            part("logo-light", solid(8, [250, 220, 40])),
            part("logo-dark", solid(8, [20, 40, 120])),
        ]
    }

    #[test]
    fn a_restyle_paints_as_a_fresh_layout_would() {
        type Parts = fn() -> Vec<postvak_core::message::InlinePart>;
        let fixtures: [(&str, &str, Parts); 2] = [
            ("newsletter", NEWSLETTER, Vec::new),
            ("schemed", SCHEMED, logos),
        ];
        for (name, html, inline) in fixtures {
            let fresh = |dark| first_strip(&mut laid_out(html, dark, inline())).pixels;
            // From either scheme to the other and back, as the theme or the Original colours toggle switches it.
            for start in [true, false] {
                let mut current = laid_out(html, start, inline());
                let Reply::Laid { generation, .. } = restyle(&mut current, 2, !start) else {
                    panic!("not laid out");
                };
                assert_eq!(generation, 2);
                assert_eq!(current.originals.is_empty(), start, "{name}");
                assert!(
                    first_strip(&mut current).pixels == fresh(!start),
                    "{name}: from dark={start} to the other"
                );
                restyle(&mut current, 3, start);
                assert_eq!(current.originals.is_empty(), !start, "{name}");
                assert!(
                    first_strip(&mut current).pixels == fresh(start),
                    "{name}: back to dark={start}"
                );
            }
        }
        // The dark scheme's own logo is the one painted after the switch, and the text on it keeps its colour.
        let mut current = laid_out(SCHEMED, false, logos());
        restyle(&mut current, 2, true);
        assert_eq!(pixel(&first_strip(&mut current), 300, 2), [20, 40, 120]);
        assert_eq!(colour_of(&current, ".logo"), rgb([0x22; 3]));
    }

    #[test]
    fn a_table_cell_shown_as_a_block_is_painted() {
        let html = "<table width=100%><tr><td style=\"display:block;background:#ff0000\">one</td></tr></table>";
        assert!(strip(html, 300.0).pixels.iter().any(|&p| is_red(p)));
    }

    /// A floated cell computes to a block too; mail that floats the cell holding the whole message laid out 42 points
    /// high.
    #[test]
    fn a_floated_table_cell_keeps_its_content() {
        let html = "<table><tr><td style=\"float:left\"><div style=\"height:2000px\">x</div></td></tr></table>";
        assert!(laid_out(html, false, Vec::new()).height >= 2000);
    }

    /// Two cells a sender made blocks inline, in a pane narrower than the table: both backgrounds and both texts are
    /// painted (#313). Whether the cells end up side by side, as `block_cells` lays them out, or stacked, as a fixed
    /// Blitz would, is left open.
    #[test]
    fn two_block_cells_in_a_narrow_pane_paint_both_backgrounds_and_their_text() {
        let html = "<table width=600 cellpadding=0 cellspacing=0><tr>\
            <td width=300 style=\"display:block;background:#ff0000;color:#ffffff\">MMMM</td>\
            <td width=300 style=\"display:block;background:#0000ff;color:#ffffff\">MMMM</td></tr></table>";
        let image = strip(html, 400.0);
        let white = at(&image, |p| p.r() > 200 && p.g() > 200 && p.b() > 200);
        let backgrounds: [(&str, &dyn Fn(Colour) -> bool); 2] = [
            ("red", &is_red),
            ("blue", &|p: Colour| p.b() > 200 && p.r() < 60 && p.g() < 60),
        ];
        for (name, test) in backgrounds {
            let painted = at(&image, test);
            let (Some(left), Some(right), Some(top), Some(bottom)) = (
                painted.iter().map(|&(x, _)| x).min(),
                painted.iter().map(|&(x, _)| x).max(),
                painted.iter().map(|&(_, y)| y).min(),
                painted.iter().map(|&(_, y)| y).max(),
            ) else {
                panic!("no {name} background");
            };
            // The white text is drawn on the background, inside its box.
            assert!(
                white
                    .iter()
                    .any(|&(x, y)| (left..=right).contains(&x) && (top..=bottom).contains(&y)),
                "no text on {name}"
            );
        }
    }

    /// Lays out and paints `html`, reporting whether an upstream crate panicked.
    fn panics(html: &str, width: f32, scale: f32) -> bool {
        panic::catch_unwind(|| {
            let mut current = None;
            let load = Request::Load(Load {
                dark: false,
                fetched: Fetched::default(),
                generation: 1,
                html: html.into(),
                inline: Vec::new(),
                scale,
                width,
            });
            handle(&mut current, load);
            for strip in 0..2 {
                handle(
                    &mut current,
                    Request::Paint {
                        generation: 1,
                        strip,
                    },
                );
            }
            // Adapted to the dark theme and back.
            for (generation, dark) in [(2, true), (3, false)] {
                handle(&mut current, Request::Restyle { generation, dark });
                handle(
                    &mut current,
                    Request::Paint {
                        generation,
                        strip: 0,
                    },
                );
            }
        })
        .is_err()
    }

    /// Found by `hostile_html_never_panics`. The render thread catches it and shows the text.
    #[test]
    #[ignore = "BUG: parley 0.11.1 line_break.rs:453 asserts on a 1e9px font in a 1px-wide layout"]
    fn a_huge_font_in_a_tiny_width_lays_out() {
        let html = "<div style=\"position:fixed;top:-1e9px\">%</table><br><style>@import url(https://x.test/i.css); * { font-size: 1e9px }</style></table>";
        assert!(!panics(html, 1.0, 1.207_759_3));
    }

    /// Found by `hostile_html_never_panics`. The render thread catches it and shows the text.
    #[test]
    #[ignore = "BUG: vello_common 0.1.0 util.rs:174 unwraps an overflowed tile bound for huge geometry"]
    fn huge_geometry_paints() {
        let html = "<div style=\"position:fixed;top:-1e9px\">aW<div style=\"float:left;width:200%;margin:-9999px\">!<style>@import url(https://x.test/i.css); * { font-size: 1e9px }</style>";
        assert!(!panics(html, 1.0, 1.946_665));
    }

    /// Pieces of hostile mail HTML: broken nesting, odd tables, every kind of URL, and CSS at its extremes.
    const PIECES: &[&str] = &[
        "<table>",
        "</table>",
        "<tr>",
        "<td colspan=0 rowspan=99999>",
        "<td style=\"display:block\">",
        "</td>",
        "<th>",
        "<tbody>",
        "<div style=\"position:fixed;top:-1e9px\">",
        "</div>",
        "<span>",
        "<p>",
        "<br>",
        "<img src=\"cid:\">",
        "<img src=\"https://x.test/a.png\" width=-1 height=1e12>",
        "<img src=\"data:image/png;base64,!!!\">",
        "<img src=\"../../../etc/passwd\">",
        "<img>",
        "<a href=\"javascript:alert(1)\">",
        "<a href=\"\">",
        "</a>",
        "<iframe src=\"https://x.test\">",
        "<link rel=stylesheet href=\"//x.test/s.css\">",
        "<base href=\"https://x.test/\">",
        "<style>@import url(https://x.test/i.css); * { font-size: 1e9px }</style>",
        "<style>td { width: 100% !important; display: block !important }</style>",
        "<style>body { columns: 0; transform: scale(0) rotate(1e9deg) }</style>",
        "<style>@font-face { font-family: x; src: url(data:font/woff2;base64,AAAA) }</style>",
        "<svg><circle r=1e9 /></svg>",
        "<math><mi>x</mi></math>",
        "<select><option>o</select>",
        "<input type=file>",
        "<textarea>",
        "<!--",
        "-->",
        "<![CDATA[",
        "&#x0;",
        "&nbsp;",
        "\u{202e}",
        "<pre>\t\t</pre>",
        "<ul><li><ol><li>",
        "<font size=+99 color=\"#zzz\">",
        "<center>",
        "<div style=\"float:left;width:200%;margin:-9999px\">",
        "<table width=600 style=\"width:auto\">",
    ];

    fn hostile_html() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        let piece = prop_oneof![
            4 => proptest::sample::select(PIECES).prop_map(str::to_string),
            1 => "[ -~]{0,40}",
            1 => ".{0,8}",
        ];
        proptest::collection::vec(piece, 0..40).prop_map(|pieces| pieces.concat())
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: std::env::var("PROPTEST_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(32),
            failure_persistence: None,
            ..Default::default()
        })]

        /// Hostile HTML through the render loop: every layout is answered, laid out or failed, and nothing takes the
        /// process down. Upstream panics the loop caught are named tests above, ignored until fixed.
        #[test]
        fn hostile_html_never_panics(
            html in hostile_html(),
            width in 1.0f32..2000.0,
            scale in 0.5f32..3.0,
            dark in proptest::bool::ANY,
        ) {
            let mut requests = vec![Request::Load(Load {
                dark,
                fetched: Fetched::default(),
                generation: 1,
                html,
                inline: Vec::new(),
                scale,
                width,
            })];
            for strip in 0..2 {
                requests.push(Request::Paint { generation: 1, strip });
            }
            for (x, y) in [(0.0, 0.0), (width / 2.0, 40.0), (-5.0, 1e9), (f32::NAN, 3.0)] {
                requests.push(Request::Hit { generation: 1, x, y });
            }
            // The other scheme and back, as the theme or the Original colours toggle switches it: dark adaptation
            // and its undoing see the hostile page too.
            for (generation, dark) in [(2, !dark), (3, dark)] {
                requests.push(Request::Restyle { generation, dark });
                requests.push(Request::Paint { generation, strip: 0 });
            }
            let got = one_by_one(requests);
            let answered = matches!(
                got.first(),
                Some(Reply::Laid { generation: 1, .. } | Reply::Failed { generation: 1 })
            );
            proptest::prop_assert!(answered, "the layout got no answer");
            for generation in [2, 3] {
                let restyled = got.iter().any(|reply| matches!(
                    reply,
                    Reply::Laid { generation: g, .. } | Reply::Failed { generation: g } if *g == generation
                ));
                proptest::prop_assert!(restyled, "restyle {} got no answer", generation);
            }
        }
    }

    #[test]
    fn physical_pixels_round_up_and_saturate() {
        assert_eq!(physical(100.4, 1.0), 101);
        assert_eq!(physical(100.0, 2.0), 200);
        assert_eq!(physical(-3.0, 1.0), 0);
        assert_eq!(physical(f32::NAN, 1.0), 0);
        assert_eq!(physical(f32::INFINITY, 1.0), u32::MAX);
    }
}
