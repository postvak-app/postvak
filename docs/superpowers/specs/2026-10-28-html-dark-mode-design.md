# HTML mail in the dark theme

Date: 2026-10-28. Status: implemented (#309). Extends `2026-10-07-postvak-html-design.md` and replaces its §5 "Page" rule (light, on a white page, in both themes) for the dark theme. Its §10 entry "Dark-mode rendering of mail that declares `color-scheme: dark` or has no colours of its own" is this.

## 1. Purpose

In the dark theme every HTML message was a bright white block in an otherwise dark window, while plain-text mail followed the theme. Dark mode is now a presentation layer over the sender's document: it never rewrites the stored or exported HTML, and it can be switched off per message and globally.

## 2. Surfaces

GUI only. The CLI and the MCP server never render HTML (they print the text part), so there is nothing for them to adapt. This is the exception AGENTS.md asks a spec to name.

## 3. Decisions

| Decision | Choice | Rejected |
|---|---|---|
| Default | Adapt in the dark theme | Off by default: most mail would stay a white block; the sender's own colours stay one click away |
| Global switch | `[ui] html_colours = "adapt"` (default) or `"original"`, read by `config.rs`; the GUI does not write it | A GUI preference: not asked for |
| Per message | "Original colours" beside Text \| HTML in the reader, and Message ▸ Show original colours (a check item); for the open message only, reset when another message opens | A persisted per-sender choice |
| The sender first | Lay out with `ColorScheme::Dark` in Blitz's `Viewport`, so `@media (prefers-color-scheme: dark)` applies | Ignoring the sender's dark CSS |
| "Already dark" | The canvas background (the root's, else the body's) when at least half opaque. When neither has one, the outermost elements within six levels below the body that have an opaque background and span at least half the page width, weighed by the area each paints: the page is dark only when the dark ones paint more than the light ones, so a dark brand header above a white article leaves the page to be adapted. Dark means light text reads better on it than dark text. A page dark by its containers keeps its surfaces and the text on them, but text on no background of the sender's sits on the dark pane and is adapted against it | Average lightness of the painted page: needs a paint pass before the decision. The first wide container alone (#382): a dark header over a light body counted as dark, leaving a white article and black footer text on the dark pane |
| Where to adapt | After style resolution, on the computed styles of the Blitz document | Rewriting the HTML or injecting CSS: changes what is parsed and would have to win over inline `!important` and `bgcolor` |
| Surfaces | Background colours and gradient stops keep hue and saturation, and their HSL lightness is mirrored around 50 % and scaled, so white becomes the dark pane's background (`Theme.night.background`, `#16181f`); greys take that background's tint; a surface already dark stays | Inverting RGB: turns brand colours into their complements |
| Text | Changed only when its contrast against the colour painted behind it (the nearest ancestor background, else the page, both after adaptation) is below 4.5:1: its lightness is mirrored, then moved towards white (or black on a light surface) just far enough to pass; hue and saturation stay. Text within 1.2:1 of a background the sender painted behind it (in the dark scheme's cascade) was hidden on purpose (preheaders) and stays hidden; with no background of the sender's behind it, light text was written for a dark canvas and is not hidden text | Recolouring all text: loses the sender's accents |
| Links | Keep the sender's colour while it passes; else the dark pane's accent (`#f3b39d`) when that passes; else as text | |
| Left alone | Raster images, background pictures that loaded and cover their box (they repeat, are sized to it, or paint at least half of it) and the colours of everything drawn over them, borders, layout. A small picture that does not repeat (an icon, a bullet) still paints, but the text beside it is adapted against the surface behind it | Darkening images: logos with a white box baked in would look worse, not better |
| No re-parse | Theme and toggle changes send `Restyle { generation, dark }`: the render thread puts the replaced styles back, changes the viewport's colour scheme, resolves styles again, takes in the background pictures only the new scheme's CSS names, and adapts; the document is not parsed again | A new `Load` per toggle |

## 4. The hook in Blitz

Blitz 0.3.0-beta.4 has no API to change computed colours after style resolution, and neither vendoring nor patching it is wanted. What it does have, and what `html/dark.rs` uses without changing Blitz:
- Every element keeps its Stylo `ElementData`, reachable through `Node::try_stylo_element_data_mut`, with the computed `ComputedValues` behind a `servo_arc::Arc`. `Arc::make_mut` gives a private copy of one node's values, and `mutate_background()` / `mutate_inherited_text()` change `background-color`, `background-image` (gradient stops only) and `color` on that copy.
- `blitz-paint` reads those values at paint time: backgrounds from the node, text from the node a glyph run's brush names. Colours do not affect layout, so no relayout is needed, and inline `!important` and `bgcolor` are already folded into the values.
- The values `adapt` replaced are kept (`Originals`) and put back before the page is styled again, so the restyle starts from the sender's values.

The approach depends on nothing restyling the document between `resolve` and paint. Postvak's render thread calls `resolve` only in `Load` and `Restyle`, and sends Blitz no hover or focus events; a hit test and a text selection do not restyle. A future Blitz that restyles on its own (animations, `:hover` from input events) would drop the adaptation of the nodes it restyles; the render tests below would show it. An upstream API for a post-cascade colour hook is the cleaner fix, and worth proposing to Blitz if this ever needs more than colours.

## 5. Flow

- `App.html_dark` is whether the window shows the dark theme. It starts from `[ui] theme`; `Live::follow_theme` reads the window's `Theme.dark` after each present (System follows the desktop, which only the window knows) and sends `UiAction::HtmlDark` when it differs. A desktop scheme change ticks the window so this runs.
- `Load.dark` is `html_dark && html_colours == adapt && !body.original_colours`. The render thread lays out with that scheme and runs `dark::adapt` when it is set; `adapt` returns at once on a page whose canvas is dark, and adapts only the text on no background on one dark by its containers.
- `HtmlState.dark` is what was last sent. When it would change (theme, toggle), the app bumps the generation and sends `Restyle`; the reply is an ordinary `Laid`, so the strips are painted again.
- The page rectangle behind the strips in `body.slint` is `Theme.night.background` when dark, white otherwise (`PageInfo.dark`).

## 6. Testing

- `html/dark.rs` unit tests: HSL round trip; a light surface turns dark and keeps hue and saturation; white becomes the page and dark surfaces stay; text on its adapted background passes 4.5:1 for typical newsletter pairs; text that reads is left alone and changed text keeps its hue; translucent text is made to read; links keep their colour while they read, else take the theme link; the theme colours read on the page.
- `html/render.rs`: a `bgcolor` cell and inline `!important` text (and `<font color>`) are adapted; a gradient turns dark and a background picture keeps its colours and the text on it, while text on one that did not load, or beside a small icon that does not repeat, is adapted; light text from the sender's dark CSS on no background stays visible and text hidden on the sender's own background stays hidden; image pixels are identical adapted or not; a page with its own dark CSS and one with a dark outer table are left alone; a dark header above a white article is adapted, footer included, while a page mostly in dark containers keeps them and adapts only its bare text; a restyle paints exactly as a fresh layout would, both ways and from either scheme, also when each scheme's CSS names its own background picture. The hostile-HTML property test and the depth tests lay out light and dark and restyle to the other scheme and back.
- `html/mod.rs` with a fake renderer: Original colours (toolbar and menu) and a theme change send `Restyle` and never a new `Load`; `html_colours = "original"` lays out light with no switch; the toggle lasts for the open message only.
- `theme.rs`: `dark::PAGE` and `dark::LINK` equal the Slint theme's `night.background` and `night.accent`.
- Snapshots: `gui_html_dark` (the newsletter adapted), `gui_html_dark_original` (Original colours on) and `gui_html_dark_own_css` (`tests/fixtures/newsletter-dark-css.html`, which brings its own dark CSS and is shown as written).

Ran on Linux x86_64; the code has no platform-specific parts.
