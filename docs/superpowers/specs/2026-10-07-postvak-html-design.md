# Postvak HTML rendering design

Date: 2026-10-07. Status: implemented. The HTML view of the reader, whose behaviour `docs/src/gui.md` describes; the code is `html` in `postvak-gui` (module map in `AGENTS.md`). Core §17's constraints (no bodies in logs, no `unwrap` outside tests) apply.

## 1. Purpose and scope

HTML-only and multipart mail render as their author laid them out, inside the reader, with no scripts and no remote content unless the user asks for it.

In scope:
- An HTML view in the body panel for messages with an HTML part, rendered by Blitz on a dedicated thread into images the window draws.
- Inline images from `cid:` parts and `data:` URIs.
- Clickable links under the existing scheme rule, with the target shown on hover.
- A key to switch between the HTML view and the text view.

Out of scope, recorded in §10: a persisted view preference, HTML in compose. Loading remote content on request, text selection and dark-mode adaptation (`2026-10-28-html-dark-mode-design.md`) came later.

## 2. Decisions log

| Decision | Choice | Rejected |
|---|---|---|
| Engine | Blitz (`blitz-dom`, `blitz-html`, `blitz-paint`) | `wry` (system webview: child windows are X11-only on Linux, needs WebKitGTK at runtime, positioned by hand each frame, invisible to snapshot tests); `servo` (JavaScript engine, monthly breaking releases, far beyond email); `litehtml` (C++17 toolchain, one maintainer, ignored `align="center"` and inline link colours in the comparison); our own HTML-to-widgets mapping (table layouts and inline CSS out of reach) |
| Raster | CPU through `anyrender_vello_cpu`, each strip a `slint::Image` | Vello on the GPU (adds a GPU path the tests cannot see) |
| Thread | One render thread owning every Blitz document; the UI thread sends jobs and receives pixels | Render on the UI thread (15–25 ms per layout stalls `j`/`k`); one thread per message (Blitz documents are not `Send`, so each would live and die on its thread anyway) |
| Tall mail | Laid out once per width, painted in 512 px strips as they scroll into view | One texture per message (long newsletters exceed GPU texture limits) |
| Remote content | Not fetched until the user asks, per message: the first layout lists the remote URLs, `html/fetch.rs` downloads them (public hosts, capped in count, size and time) and the page is laid out again from memory; `cid:` and `data:` always resolve | Fetch by default; per-sender allow list now; fetching inside the net provider (the render thread would wait on the network) |
| Colour scheme | Light, on a white page, in the light theme; in the dark theme adapted after style resolution, see `2026-10-28-html-dark-mode-design.md` | Follow the app theme by scheme alone (most mail hardcodes dark text on no background) |
| Default view | HTML when the message has an HTML part, else text | Text first; remember per sender |
| Versions | Exact pins (`=0.3.0-beta.4`) for the Blitz crates and the matching `anyrender` | Caret ranges (the beta crates already disagree on `anyrender` between minor versions) |

Measured on one newsletter fixture at 700 px, Linux x86_64, 4 cores, release build: first document 24 ms parse, style and layout, later ones 15 ms, paint 5 ms. A standalone renderer binary is 22 MB over 288 crates. Ran on Linux; not yet on macOS.

`wry` on Wayland, rechecked 2026-10-13: still out. `build_as_child` and `WebView::new_as_child` are documented as X11-only. wry's Wayland route, `build_gtk` or `new_gtk` inside a `gtk::Fixed`, needs a GTK-owned window and GTK's main loop; the app's window is winit's. The open GTK4 port (wry#1767, last updated 2026-08-07) adds a `wayland` feature that embeds only into GTK4-owned windows, found by matching `wl_surface`. The workarounds each lose more than Blitz costs:
- Forcing the window onto X11 under XWayland: blurry fractional scaling and an X11 server at runtime, and the other objections above still hold.
- A separate GTK popup: Wayland clients cannot position their own toplevels, so it cannot sit in the body panel.
- A GTK main window: replaces the toolkit.
- Offscreen WebKitGTK snapshots: not exposed by wry, and the result is still a static image.

## 3. Packaging

- No feature of its own: the Blitz crates, `html5ever`, `anyrender`, `anyrender_vello_cpu`, `data-url`, `fontique` and `ureq` are dependencies of `postvak-gui`, so the `gui` feature brings HTML mail. `data-url` decodes `data:` URIs; it is already in the tree through `usvg`. `html5ever` parses mail as HTML even when it declares the XHTML namespace, which Blitz's own entry point would hand to its XML parser. `fontique` adds symbol fonts to the fallbacks and, on Linux, turns on `fontconfig-dlopen`, so fontconfig is loaded at runtime rather than linked. `ureq` downloads remote content when the user asks.
- Stylo, Blitz's CSS engine, is MPL-2.0. File-level copyleft: shipping a binary that links it is fine; changes to its own files would be published. Noted in the licence section of `docs/src/index.md`, and so of `README.md`.
- `cargo deny` and `cargo machete` cover the new crates as they cover the rest.

## 4. Architecture

`message::html_body` (core) picks the first `text/html` part and every part with a `Content-ID`; nothing is stored beyond the raw message. In `postvak-gui`, `html/mod.rs` holds the per-message state, the request and reply types and strip selection, `html/render.rs` the render thread that owns every Blitz document, and `html/net.rs` the net provider; the reader's state changes only in `state.rs`, from `UiAction`s and the thread's replies. Each request carries a generation the window bumps for every new message or width, replies with an old one are dropped, and a `Load` replaces whatever was pending, so moving through mail with `j` never queues layouts.

## 5. Rendering

- **Width.** The body panel's inner width in points, at least 320; the page itself is laid out at no less than 601 (see Block cells). A width change re-lays out once the width has held still for 100 ms; meanwhile the old strips are drawn as they are, clipped or padded. A page wider than the panel (a fixed 600 px table in a 550 px panel) is laid out at its own width and scrolls sideways.
- **Scale.** The window's scale factor becomes Blitz's hidpi scale; strips are painted at physical pixels and drawn at points.
- **Strips.** 512 physical pixels tall, the full width, each painted with Blitz's viewport scrolled to its top; Blitz's paint offsets only place the document on the canvas. The view asks for the strips intersecting the visible rect plus one screen above and below, those in view first, and drops textures more than three screens away. A new message's page appears once it is laid out and the strips in view are painted, so it shows whole rather than as an empty page its strips fill; until then the view shows no page, and "Rendering…" in its place if that takes longer than 300 ms. A later layout of the same message (a new width, the theme, remote content) keeps the page on screen, and a strip not yet painted draws as the page's background.
- **Page.** In the light theme a white page the width of the panel, the document painted over it with `ColorScheme::Light`. The rest of the panel follows the app theme. In the dark theme the page is the dark pane's background and the document is styled with `ColorScheme::Dark` and adapted (`2026-10-28-html-dark-mode-design.md`).
- **User agent CSS.** Blitz's own, plus `img { max-width: 100%; height: auto }` and `body { overflow-wrap: anywhere }`, so images and long URLs fit a narrow panel.
- **Base URL.** `postvak://mail/`. Blitz panics on a relative URL it cannot resolve against its default base; this one resolves them to URLs the net provider refuses.
- **Fonts.** System fonts through Blitz's `FontContext`, scanned once per render thread and cloned for each message: a clone shares the scanned fonts, and what a mail's `@font-face` adds stays in its own copy. A panic on the thread drops them, so they are scanned again. Tests build the context from a bundled font (Hanken Grotesk), so snapshots do not depend on the machine.
- **Limits.** HTML over 2 MiB, or a document taller than 200,000 px, shows the text view with the note "Too large to render; showing text."

## 6. Resources and privacy

- The net provider answers `cid:` from the message's `Content-ID` parts and `data:` by decoding the URI. Remote URLs the user chose to load answer from the downloaded bytes; everything else, including `file`, completes at once as a failure, so the render thread never waits on the network.
- Blocked images keep their `width`/`height` box. When the page asked for anything remote (an image, a stylesheet, an `<iframe>`), a line above the page says "Remote content not loaded." with a "Load remote content" button.
- Blitz runs no scripts; `<form>` controls draw but are never submitted, since the view sends Blitz no input events beyond hit tests.
- Nothing from the body is logged. Upstream panic messages can quote the mail (a URL Blitz could not resolve), so a panic hook keeps them off stderr for the render thread and logs only that rendering failed; other threads keep the default hook.

## 7. Interaction

- **Links.** When the pointer moves the view sends `Hit` for its position; the reply's `href`, if any, shows as a tooltip and the pointer becomes a hand. A click opens it through `opener` only when it is an absolute `http`, `https` or `mailto` URL, the same rule as the text view; anything else, relative URLs included, does nothing.
- **Toggle.** `v` switches the current message between the HTML and text views. The choice lasts until the next message is shown. Added to the `?` key table. The text view is the existing one. Text in the HTML view can be selected and copied too.
- **Scrolling.** The page scrolls with the reader as the text view does. Wider-than-panel content scrolls horizontally.
- **Read state.** Unchanged: the 1 s timer starts when the message is on screen, whichever view shows it.

## 8. Failure

- The render thread wraps each job in `catch_unwind`. A panic, or `Failed`, shows the text view with "Could not render HTML; showing text." for that message, and the thread drops the document and carries on. If the thread itself has gone, the next `Load` restarts it.
- A message whose body is not downloaded shows "Loading…" as today; the HTML view starts when `BodyReady` arrives.

## 9. Testing

**Unit,** in `message` and `gui::html`:
- `html_body`: `text/html` only, `multipart/alternative`, `multipart/related` with two `cid:` images, plain text only (`None`).
- Net provider: `cid:` and `data:` resolve; `https:`, `http:`, `file:` and `@import` fail without a network call.
- Render thread: the newsletter lays out, its first two strips paint (the second is not blank), and a point over its button finds the link; a stale generation gets no answer; a strip past the end is not painted; relative URLs and an empty page do not panic; two `Load`s in a row lay out only the second; a panic replies `Failed`.
- Strip selection for a visible rect, and eviction three screens away; which links may open.

**GUI,** in the window under Slint's testing backend: the HTML and text views and `v`, link hover and which links open, the remote-content line, the failure and size notes, and stale replies; snapshots in `snapshots.rs` cover the light and dark themes and a narrow pane.

**Fuzz:** `tests/fuzz.rs` puts `html_body` beside the other message parsers. `hostile_html_never_panics` in `render.rs` (it needs the private render loop) runs hostile HTML through layout, paint and hit tests in the render loop and asserts every layout is answered. Upstream panics it found are named tests there, ignored until fixed: a Parley line-break assertion on a 1e9 px font in a 1 px layout, and a `vello_common` overflow painting huge geometry. Both are caught on the render thread and show the text.

**Legacy attributes:** old mail leans on presentational attributes Blitz 0.3.0-beta.4 reads wrongly or not at all. `html/depth.rs` rewrites `bgcolor` (and `<font color>`) to `#rrggbb` while parsing, by the HTML "rules for parsing a legacy colour value" (`html/legacy.rs`: names, `#rgb` doubled, bare hex, the lenient fallback for junk; empty and `transparent` drop the attribute), since Blitz reads `#ddd` as near black and ignores names and bare hex. `MAIL_CSS` maps `border` on a table to 1px cell borders (not `border="0"`), `valign` on `td`/`th`/`tr`, `align="center"` on a table, `align="left|right"` on an image and `size="1".."7"` on `<font>` (not `+1`/`-1`) as user-agent CSS, so author CSS wins. Drop each once a pinned Blitz does the same. Tests: one pixel test per attribute in `render.rs`, unit tests for the colour rules.

**Block cells (#313):** Blitz 0.3.0-beta.4 lays a `<td>` or `<th>` with `display: block` out one line high and paints nothing of it, and responsive mail stacks its columns that way. Two workarounds in `render.rs` keep the content. After the first style pass, `block_cells` finds the cells styled as anything but a cell or hidden, and the page is styled again from scratch with an inline `display: table-cell !important` on each, so the columns stay side by side and a cell the sender hid stays hidden. And a page is laid out at 601 px or wider (`MIN_WIDTH`), so mobile queries up to 600 px, which also hide parts and resize text, never match and such mail keeps its whole desktop layout; a narrower pane scrolls sideways (see Width above), and simple mail no longer reflows below 601 px. Mail with block cells is parsed and styled twice. Remove both once a pinned Blitz lays out block cells (the upstream Blitz report is still to be filed); `a_table_cell_shown_as_a_block_is_painted` and `two_block_cells_in_a_narrow_pane_paint_both_backgrounds_and_their_text` must still pass without them, since neither checks where the cells go.

Ran on Linux (CPU raster needs no GPU, so CI covers it); macOS reported separately as compiled on or ran on.

## 10. Later

- Remembering a per-sender choice to load remote content.
- Remembering the view choice in `[ui]`.
- Vello on the GPU if CPU paint shows up in profiles.
