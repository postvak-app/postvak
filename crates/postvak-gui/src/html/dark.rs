//! HTML mail in the dark theme: the page is styled with the dark colour scheme, so the sender's own dark-mode CSS
//! applies, and a page that is still light is adapted after style resolution. Light surfaces become dark ones of the
//! same hue, and text that no longer reads on what is painted behind it is made lighter. Only the computed styles of
//! the document change: the stored HTML is never rewritten, and `restore` puts the sender's colours back.
use std::collections::HashSet;

use blitz_dom::node::{ImageData, Status};
use blitz_dom::{BaseDocument, NodeId, local_name};
use style::color::{AbsoluteColor, ColorSpace};
use style::properties::ComputedValues;
use style::properties::style_structs::Background;
use style::servo_arc::Arc;
use style::values::computed::background::BackgroundSize;
use style::values::computed::{Color, Gradient, Image, Length, NonNegativeLengthPercentage};
use style::values::generics::image::GradientItem;
use style::values::generics::length::GenericLengthPercentageOrAuto;
use style::values::specified::background::{BackgroundRepeat, BackgroundRepeatKeyword};

use crate::theme::contrast;

/// An sRGB colour, each channel 0 to 1.
pub(crate) type Rgb = [f32; 3];
/// An sRGB colour with straight alpha, each channel 0 to 1.
pub(crate) type Rgba = [f32; 4];

/// The dark page behind a message, `Theme.night.background`: a white page becomes this.
pub(crate) const PAGE: Rgb = rgb8(0x16, 0x18, 0x1f);
/// Links that no longer read on the dark page, `Theme.night.accent`.
pub(crate) const LINK: Rgb = rgb8(0xf3, 0xb3, 0x9d);
/// WCAG AA for body text.
const AA: f32 = 4.5;
/// Text this close to a background the sender painted behind it was hidden on purpose (a preheader, a spam trap): it
/// stays hidden.
const HIDDEN: f32 = 1.2;
/// Below this HSL saturation a colour counts as grey and takes the page's tint.
const GREY: f32 = 0.05;
/// How far below the body the outermost container of a page is looked for.
const CONTAINER_DEPTH: usize = 6;
/// The share of its box's width a background picture must paint for the text in the box to count as written on the
/// picture: below it, the picture is an icon or a bullet beside the text. Width, not area: a hero picture above the
/// body copy of a taller cell paints less than half its area but is under the headline.
const COVERS: f32 = 0.5;

const WHITE: Rgb = [1.0; 3];
const BLACK: Rgb = [0.0; 3];

const fn rgb8(r: u8, g: u8, b: u8) -> Rgb {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

/// The computed styles `adapt` replaced, by node, to put back before the page is styled again.
pub(crate) type Originals = Vec<(NodeId, Arc<ComputedValues>)>;

/// Adapts a page styled with the dark scheme, unless its own colours already make it dark; the styles it replaced.
/// A page made dark by its containers rather than its canvas keeps its colours, except text on no background of the
/// sender's, which sits on the dark pane.
pub(crate) fn adapt(doc: &mut BaseDocument) -> Originals {
    let bare = match judge(doc) {
        Page::DarkCanvas => return Vec::new(),
        Page::DarkContainers => true,
        Page::Light => false,
    };
    let changes = plan(doc, bare);
    apply(doc, changes)
}

/// Puts back the styles `adapt` replaced.
pub(crate) fn restore(doc: &mut BaseDocument, originals: Originals) {
    for (id, style) in originals {
        let Some(data) = doc
            .get_node_mut(id)
            .and_then(|node| node.try_stylo_element_data_mut())
        else {
            continue;
        };
        if let Some(mut data) = data.get_mut() {
            data.styles.primary = Some(style);
        }
    }
}

/// A light surface as its dark counterpart: hue and saturation kept, lightness mirrored around the middle and scaled
/// so white becomes the page. A surface already dark stays as it is; a grey takes the page's tint.
pub(crate) fn surface(colour: Rgb) -> Rgb {
    let [h, s, l] = hsl(colour);
    if l <= 0.5 {
        return colour;
    }
    let [page_h, page_s, page_l] = hsl(PAGE);
    let dark = 0.5 - (l - 0.5) / 0.5 * (0.5 - page_l);
    if s < GREY {
        from_hsl([page_h, page_s, dark])
    } else {
        from_hsl([h, s, dark])
    }
}

/// Text in `fg` on `bg`, changed only when it falls below AA: its lightness mirrored, then moved towards white (or
/// black, on a light background) just far enough to pass; hue and saturation stay.
pub(crate) fn text(fg: Rgba, bg: Rgb) -> Option<Rgba> {
    if fg[3] <= 0.0 || contrast(over(fg, bg), bg) >= AA {
        return None;
    }
    let [h, s, l] = hsl([fg[0], fg[1], fg[2]]);
    let lighter = contrast(WHITE, bg) >= contrast(BLACK, bg);
    let (from, pole) = if lighter {
        (l.max(1.0 - l), 1.0)
    } else {
        (l.min(1.0 - l), 0.0)
    };
    // Translucent text that cannot pass even at the pole is made opaque.
    let pick = |alpha: f32, l: f32| {
        let [r, g, b] = from_hsl([h, s, l]);
        [r, g, b, alpha]
    };
    let passes = |c: Rgba| contrast(over(c, bg), bg) >= AA;
    let alpha = if passes(pick(fg[3], pole)) {
        fg[3]
    } else {
        1.0
    };
    if passes(pick(alpha, from)) {
        return Some(pick(alpha, from));
    }
    let (mut fails, mut works) = (from, pole);
    for _ in 0..24 {
        let mid = (fails + works) / 2.0;
        if passes(pick(alpha, mid)) {
            works = mid;
        } else {
            fails = mid;
        }
    }
    Some(pick(alpha, works))
}

/// A link in `fg` on `bg`: the sender's colour while it reads, else the theme's link colour, else as `text`.
pub(crate) fn link(fg: Rgba, bg: Rgb) -> Option<Rgba> {
    if fg[3] <= 0.0 || contrast(over(fg, bg), bg) >= AA {
        None
    } else if contrast(LINK, bg) >= AA {
        Some([LINK[0], LINK[1], LINK[2], 1.0])
    } else {
        text(fg, bg)
    }
}

/// Whether light text reads better on `colour` than dark text.
fn is_dark(colour: Rgb) -> bool {
    contrast(colour, WHITE) > contrast(colour, BLACK)
}

/// `colour` painted over `below`.
fn over(colour: Rgba, below: Rgb) -> Rgb {
    let a = colour[3].clamp(0.0, 1.0);
    [0, 1, 2].map(|i| colour[i] * a + below[i] * (1.0 - a))
}

fn hsl([r, g, b]: Rgb) -> [f32; 3] {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON {
        return [0.0, 0.0, l];
    }
    let s = (d / (1.0 - (2.0 * l - 1.0).abs())).min(1.0);
    let sector = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [sector * 60.0, s, l]
}

fn from_hsl([h, s, l]: [f32; 3]) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let sector = (h / 60.0).rem_euclid(6.0);
    let x = c * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r + m, g + m, b + m].map(|v| v.clamp(0.0, 1.0))
}

/// A computed colour in sRGB; components a colour space leaves undefined count as 0.
fn rgba(colour: &AbsoluteColor) -> Rgba {
    let c = colour.to_color_space(ColorSpace::Srgb);
    c.raw_components()
        .map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) })
}

fn absolute(c: Rgba) -> AbsoluteColor {
    AbsoluteColor::new(ColorSpace::Srgb, c[0], c[1], c[2], c[3])
}

/// What a node paints: its text colour, its background colour, and the images over that background.
struct Paint {
    fg: Rgba,
    bg: Rgba,
    layers: Layers,
}

/// A node's background images.
enum Layers {
    None,
    /// Only gradients, with every colour stop: they are adapted as surfaces are.
    Gradients(Vec<Rgba>),
    /// A picture that loaded and covers the box (or anything else that is not a gradient or a picture), left as the
    /// sender drew it, with what is on it.
    Picture,
}

fn paint_of(node: &blitz_dom::Node) -> Option<Paint> {
    let style = node.primary_styles()?;
    let current = style.clone_color();
    let back = style.get_background();
    let bg = back.background_color.resolve_to_absolute(&current);
    let mut stops = Vec::new();
    let mut picture = false;
    let loaded = node
        .element_data()
        .map(|el| &el.background_images[..])
        .unwrap_or_default();
    for (idx, layer) in back.background_image.0.iter().enumerate() {
        match layer {
            Image::None => {}
            Image::Gradient(gradient) => {
                for_each_stop(gradient, |stop| {
                    stops.push(rgba(&stop.resolve_to_absolute(&current)))
                });
            }
            // A picture that did not load (remote content not shown, a missing part) paints nothing; one that paints
            // only a corner of the box (an icon, a bullet) leaves the text on what is behind it.
            Image::Url(_) => {
                picture |= loaded
                    .get(idx)
                    .and_then(Option::as_ref)
                    .filter(|data| {
                        data.status == Status::Ok && !matches!(data.image, ImageData::None)
                    })
                    .is_some_and(|data| covers(node, back, idx, &data.image))
            }
            _ => picture = true,
        }
    }
    let layers = if picture {
        Layers::Picture
    } else if stops.is_empty() {
        Layers::None
    } else {
        Layers::Gradients(stops)
    };
    Some(Paint {
        fg: rgba(&current),
        bg: rgba(&bg),
        layers,
    })
}

/// The `idx`th value of a background list, which repeats when it is shorter than the list of images.
fn cyclic<T>(list: &[T], idx: usize) -> Option<&T> {
    list.get(idx.checked_rem(list.len())?)
}

/// Whether the `idx`th background picture of `node`, `image`, is what the text in its box is on: it paints at least
/// `COVERS` of the box's width, as its size and repeat lay it out. A picture without a size of its own, or a box without one, counts as covering. `background-position` is not
/// read: a picture placed partly outside its box counts as painting all of its tile, the side that keeps the sender's
/// colours.
fn covers(node: &blitz_dom::Node, back: &Background, idx: usize, image: &ImageData) -> bool {
    let size = node.final_layout().size;
    let (w, h) = (size.width, size.height);
    let ImageData::Raster(raster) = image else {
        return true;
    };
    let (iw, ih) = (raster.width as f32, raster.height as f32);
    if !(w > 0.0 && h > 0.0 && iw > 0.0 && ih > 0.0) {
        return true;
    }
    let resolve = |value: &GenericLengthPercentageOrAuto<NonNegativeLengthPercentage>,
                   against: f32| match value {
        GenericLengthPercentageOrAuto::LengthPercentage(v) => {
            Some(v.0.resolve(Length::new(against)).px())
        }
        GenericLengthPercentageOrAuto::Auto => None,
    };
    let (tile_w, tile_h) = match cyclic(&back.background_size.0, idx) {
        Some(BackgroundSize::Cover) => return true,
        Some(BackgroundSize::Contain) => {
            let ratio = (w / iw).min(h / ih);
            (iw * ratio, ih * ratio)
        }
        Some(BackgroundSize::ExplicitSize { width, height }) => {
            match (resolve(width, w), resolve(height, h)) {
                (Some(tw), Some(th)) => (tw, th),
                (Some(tw), None) => (tw, tw * ih / iw),
                (None, Some(th)) => (th * iw / ih, th),
                (None, None) => (iw, ih),
            }
        }
        None => (iw, ih),
    };
    let (across, down) = cyclic(&back.background_repeat.0, idx).map_or(
        (
            BackgroundRepeatKeyword::Repeat,
            BackgroundRepeatKeyword::Repeat,
        ),
        |BackgroundRepeat(across, down)| (*across, *down),
    );
    // A repeating picture fills its axis; one that does not, as much of it as one tile reaches.
    let extent = |keyword: BackgroundRepeatKeyword, tile: f32, side: f32| match keyword {
        BackgroundRepeatKeyword::NoRepeat => tile.clamp(0.0, side),
        _ => side,
    };
    // Painting nothing down the box (a zero-height tile) paints nothing at all.
    extent(across, tile_w, w) >= COVERS * w && extent(down, tile_h, h) > 0.0
}

/// Calls `f` on each colour stop of `gradient`.
fn for_each_stop(gradient: &Gradient, mut f: impl FnMut(&Color)) {
    fn stop<T>(item: &GradientItem<Color, T>) -> Option<&Color> {
        match item {
            GradientItem::SimpleColorStop(colour)
            | GradientItem::ComplexColorStop { color: colour, .. } => Some(colour),
            GradientItem::InterpolationHint(_) => None,
        }
    }
    match gradient {
        Gradient::Linear { items, .. } | Gradient::Radial { items, .. } => {
            items.iter().filter_map(stop).for_each(&mut f);
        }
        Gradient::Conic { items, .. } => items.iter().filter_map(stop).for_each(&mut f),
    }
}

/// Every colour stop of `gradient` as a dark surface, resolved against `current`.
fn adapt_stops(gradient: &mut Gradient, current: &AbsoluteColor) {
    fn stop<T>(item: &mut GradientItem<Color, T>, current: &AbsoluteColor) {
        if let GradientItem::SimpleColorStop(colour)
        | GradientItem::ComplexColorStop { color: colour, .. } = item
        {
            let c = rgba(&colour.resolve_to_absolute(current));
            let [r, g, b] = surface([c[0], c[1], c[2]]);
            *colour = Color::Absolute(absolute([r, g, b, c[3]]));
        }
    }
    match gradient {
        Gradient::Linear { items, .. } | Gradient::Radial { items, .. } => {
            items.iter_mut().for_each(|item| stop(item, current));
        }
        Gradient::Conic { items, .. } => items.iter_mut().for_each(|item| stop(item, current)),
    }
}

/// The mean of `colours`, as what a gradient of them paints on average.
fn mean(colours: &[Rgba]) -> Rgba {
    let n = colours.len().max(1) as f32;
    [0, 1, 2, 3].map(|i| colours.iter().map(|c| c[i]).sum::<f32>() / n)
}

/// What a page's own colours make it.
#[derive(Debug, PartialEq)]
enum Page {
    Light,
    /// Its canvas (the root's background, else the body's) is dark.
    DarkCanvas,
    /// It has no canvas, and dark containers spanning at least half the page paint more of it than light ones.
    DarkContainers,
}

/// What the page's own colours make it: the canvas decides, or, when there is none, the outermost containers that span
/// at least half the page, by the area each paints. A dark header above a white article leaves the page light.
fn judge(doc: &BaseDocument) -> Page {
    let root = doc.root_element();
    let body = doc
        .query_selector("body")
        .ok()
        .flatten()
        .and_then(|id| doc.get_node(id));
    let opaque = |node: &blitz_dom::Node| {
        paint_of(node)
            .map(|paint| paint.bg)
            .filter(|bg| bg[3] >= 0.5)
    };
    if let Some(bg) = opaque(root).or_else(|| body.and_then(opaque)) {
        return if is_dark([bg[0], bg[1], bg[2]]) {
            Page::DarkCanvas
        } else {
            Page::Light
        };
    }
    let Some(body) = body else {
        return Page::Light;
    };
    let half = root.final_layout().size.width / 2.0;
    let (mut dark, mut light) = (0.0_f32, 0.0_f32);
    let mut level = vec![body.id];
    for _ in 0..CONTAINER_DEPTH {
        let mut next = Vec::new();
        for node in level.iter().filter_map(|&id| doc.get_node(id)) {
            for child in node.children.iter().filter_map(|&id| doc.get_node(id)) {
                if !child.is_element() {
                    continue;
                }
                let size = child.final_layout().size;
                if let Some(bg) = opaque(child)
                    && size.width >= half
                {
                    // What is inside a container is on it: its own containers do not count again.
                    let area = (size.width * size.height).max(0.0);
                    if is_dark([bg[0], bg[1], bg[2]]) {
                        dark += area;
                    } else {
                        light += area;
                    }
                    continue;
                }
                next.push(child.id);
            }
        }
        level = next;
    }
    if dark > light {
        Page::DarkContainers
    } else {
        Page::Light
    }
}

/// What is painted behind a node, before and after adaptation, and what it sits in.
#[derive(Clone, Copy)]
struct Behind {
    original: Rgb,
    adapted: Rgb,
    /// Inside a box with a background image, whose colours are left as the sender drew them.
    image: bool,
    link: bool,
    /// The sender painted a background behind the node, in the dark scheme's cascade. Without one, `original` is only
    /// the white a light page is assumed to have, and light text may be written for a dark canvas the sender expects.
    painted: bool,
}

/// A node's new background and text colours, and whether its gradients turn dark.
struct Change {
    node: NodeId,
    background: Option<Rgba>,
    colour: Option<Rgba>,
    gradients: bool,
}

/// The changes for every element and anonymous box, top-down, each against what its ancestors paint behind it. With
/// `bare`, only text on no background of the sender's changes, against the dark pane; surfaces stay as they are.
fn plan(doc: &BaseDocument, bare: bool) -> Vec<Change> {
    let page = Behind {
        original: WHITE,
        adapted: PAGE,
        image: false,
        link: false,
        painted: false,
    };
    let mut stack = vec![(doc.root_element().id, page)];
    let mut seen = HashSet::new();
    let mut changes = Vec::new();
    while let Some((id, outside)) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        let mut inside = outside;
        if let Some(Paint { fg, bg, layers }) = paint_of(node) {
            let mut change = Change {
                node: id,
                background: None,
                colour: None,
                gradients: false,
            };
            if outside.image || matches!(layers, Layers::Picture) {
                inside.image = true;
            } else {
                if bg[3] > 0.0 {
                    let [r, g, b] = surface([bg[0], bg[1], bg[2]]);
                    let dark = [r, g, b, bg[3]];
                    change.background = (!bare).then_some(dark);
                    inside.original = over(bg, outside.original);
                    inside.adapted = over(dark, outside.adapted);
                    inside.painted = true;
                }
                if let Layers::Gradients(stops) = layers {
                    let dark: Vec<Rgba> = stops
                        .iter()
                        .map(|c| {
                            let [r, g, b] = surface([c[0], c[1], c[2]]);
                            [r, g, b, c[3]]
                        })
                        .collect();
                    change.gradients = !bare;
                    inside.original = over(mean(&stops), inside.original);
                    inside.adapted = over(mean(&dark), inside.adapted);
                    inside.painted = true;
                }
                inside.link = outside.link
                    || (node.data.is_element_with_tag_name(&local_name!("a"))
                        && node.attr(local_name!("href")).is_some());
                change.colour = if bare && inside.painted {
                    None
                } else if inside.painted
                    && fg[3] > 0.0
                    && contrast(over(fg, inside.original), inside.original) < HIDDEN
                {
                    let [r, g, b] = inside.adapted;
                    Some([r, g, b, fg[3]])
                } else if inside.link {
                    link(fg, inside.adapted)
                } else {
                    text(fg, inside.adapted)
                };
            }
            if change.background.is_some() || change.colour.is_some() || change.gradients {
                changes.push(change);
            }
        }
        let laid = node.layout_children.borrow();
        let children = node
            .children
            .iter()
            .chain(laid.iter().flat_map(|c| c.iter()));
        stack.extend(children.map(|&child| (child, inside)));
    }
    changes
}

fn apply(doc: &mut BaseDocument, changes: Vec<Change>) -> Originals {
    let mut originals = Vec::with_capacity(changes.len());
    for change in changes {
        let Some(data) = doc
            .get_node_mut(change.node)
            .and_then(|node| node.try_stylo_element_data_mut())
        else {
            continue;
        };
        let Some(mut data) = data.get_mut() else {
            continue;
        };
        let Some(primary) = data.styles.primary.as_mut() else {
            continue;
        };
        originals.push((change.node, primary.clone()));
        let style = Arc::make_mut(primary);
        if let Some(background) = change.background {
            style.mutate_background().background_color = Color::Absolute(absolute(background));
        }
        if change.gradients {
            let current = style.clone_color();
            let mut layers = style.get_background().background_image.clone();
            for layer in layers.0.iter_mut() {
                if let Image::Gradient(gradient) = layer {
                    adapt_stops(gradient, &current);
                }
            }
            style.mutate_background().background_image = layers;
        }
        if let Some(colour) = change.colour {
            style.mutate_inherited_text().color = absolute(colour);
        }
    }
    originals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(v: u32) -> Rgb {
        rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    fn opaque(c: Rgb) -> Rgba {
        [c[0], c[1], c[2], 1.0]
    }

    fn rgb(c: Rgba) -> Rgb {
        [c[0], c[1], c[2]]
    }

    #[test]
    fn hsl_round_trips() {
        for c in [
            0x123456, 0x0b6efd, 0xf2f4f6, 0xff0000, 0x00ff00, 0x0000ff, 0xffffff, 0x000000,
            0x888888,
        ] {
            let back = from_hsl(hsl(hex(c)));
            for (a, b) in back.iter().zip(hex(c)) {
                assert!((a - b).abs() < 1e-4, "{c:06x}: {back:?}");
            }
        }
    }

    #[test]
    fn a_light_surface_turns_dark_and_keeps_its_hue() {
        for c in [0xfde8e8, 0xe8f0fe, 0xfff4cc, 0xd1fae5, 0xf2f4f6, 0x0b6efd] {
            let [h, s, l] = hsl(hex(c));
            let dark = surface(hex(c));
            let [dh, ds, dl] = hsl(dark);
            if l > 0.5 {
                assert!(dl < 0.5, "{c:06x} still light: {dl}");
                assert!(is_dark(dark), "{c:06x}");
            } else {
                assert_eq!(dark, hex(c), "{c:06x} was dark already");
            }
            assert!((dh - h).abs() < 0.5, "{c:06x}: hue {h} became {dh}");
            assert!((ds - s).abs() < 0.01, "{c:06x}: saturation {s} became {ds}");
        }
    }

    #[test]
    fn white_becomes_the_page_and_dark_surfaces_stay() {
        let page = surface(WHITE);
        for (a, b) in page.iter().zip(PAGE) {
            assert!((a - b).abs() < 1e-3, "{page:?}");
        }
        assert_eq!(surface(hex(0x123456)), hex(0x123456));
        assert_eq!(surface(BLACK), BLACK);
    }

    #[test]
    fn text_on_its_adapted_background_reads_for_typical_newsletter_pairs() {
        // Text and the background it was written for, as newsletters pair them.
        let pairs = [
            (0x222222, 0xffffff),
            (0x555555, 0xffffff),
            (0x888888, 0xffffff),
            (0x000000, 0xf2f4f6),
            (0x1a1a1a, 0xfafafa),
            (0x0b6efd, 0xffffff),
            (0xd93025, 0xfff4f4),
            (0x6b7280, 0xf9fafb),
            (0x1e3a8a, 0xe8f0fe),
            (0x065f46, 0xd1fae5),
            (0xffffff, 0x123456),
            (0xffffff, 0x0b6efd),
            (0x333333, 0xfff4cc),
        ];
        for (fg, bg) in pairs {
            let behind = surface(hex(bg));
            let shown = text(opaque(hex(fg)), behind).map_or(hex(fg), rgb);
            let ratio = contrast(shown, behind);
            assert!(ratio >= AA, "{fg:06x} on {bg:06x}: {ratio}");
        }
    }

    #[test]
    fn text_that_reads_is_left_alone_and_changed_text_keeps_its_hue() {
        assert_eq!(text(opaque(WHITE), hex(0x123456)), None);
        assert_eq!(text(opaque(hex(0xe9e5dc)), PAGE), None);
        let blue = hex(0x1e3a8a);
        let lighter = text(opaque(blue), PAGE).map(rgb).unwrap();
        assert!((hsl(lighter)[0] - hsl(blue)[0]).abs() < 0.5);
        assert!(hsl(lighter)[2] > hsl(blue)[2]);
    }

    #[test]
    fn translucent_text_is_made_to_read() {
        let faint = [0.0, 0.0, 0.0, 0.3];
        let shown = text(faint, PAGE).unwrap();
        assert!(contrast(over(shown, PAGE), PAGE) >= AA);
        assert_eq!(text([0.0, 0.0, 0.0, 0.0], PAGE), None);
    }

    #[test]
    fn a_link_keeps_its_colour_while_it_reads_and_else_takes_the_theme_link() {
        let light_blue = opaque(hex(0x8ab4f8));
        assert_eq!(link(light_blue, PAGE), None);
        let dark_blue = opaque(hex(0x0b3d91));
        assert_eq!(link(dark_blue, PAGE), Some(opaque(LINK)));
        // On a surface where the theme's link would not read either, the link is lightened like text.
        let mid = hex(0xc05a3a);
        let shown = link(opaque(hex(0xb05030)), mid).map_or(hex(0xb05030), rgb);
        assert!(contrast(shown, mid) >= AA);
    }

    #[test]
    fn the_theme_colours_read_on_the_page() {
        assert!(contrast(LINK, PAGE) >= AA);
        assert!(is_dark(PAGE));
    }
}
