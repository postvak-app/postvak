//! The HTML of a reply or forward of an HTML original: the user's text as paragraphs, then the original as close to
//! how it arrived as is safe to send on. The original goes through html5ever's tokenizer rather than its tree
//! builder: the tokenizer takes linear time however deep the markup nests, and one pass both drops what must not
//! travel and keeps the tags balanced, so nothing in the original can close the quote around it.

use std::cell::RefCell;

use html5ever::TokenizerResult;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

use super::build::{HtmlPart, InlineImage};

/// The quote around a replied original: `type="cite"` for Apple Mail and Thunderbird, the class for Gmail, the style
/// for Outlook.
pub const QUOTE_OPEN: &str = r#"<div class="gmail_quote"><blockquote type="cite" class="gmail_quote" style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex">"#;
const QUOTE_CLOSE: &str = "</blockquote></div>";

/// Open elements past this depth make the original too deep to keep as HTML; it is quoted as text instead.
const MAX_DEPTH: usize = 512;

/// The original's HTML and the images its `cid:` URLs name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Original {
    pub html: String,
    pub inline: Vec<InlineImage>,
}

/// The original's `<style>` elements, then the content of its `<body>`, without what must not travel: `script`,
/// `iframe`, `object`, `embed`, `form`, `meta`, `base` and the rest of the head, every `on…` attribute and every
/// `javascript:` URL. Nothing else is rewritten; remote images stay remote. None when the markup nests deeper than
/// is sane for mail.
pub fn clean_body(html: &str) -> Option<String> {
    let sink = Cleaner::default();
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(html));
    while !matches!(tokenizer.feed(&queue), TokenizerResult::Done) {}
    tokenizer.end();
    let state = tokenizer.sink.state.into_inner();
    if state.too_deep {
        return None;
    }
    let mut out = state.styles;
    out.push_str(&state.body);
    for name in state.open.iter().rev() {
        out.push_str(&format!("</{name}>"));
    }
    Some(out)
}

/// The user's text as HTML: escaped, a `<p>` per paragraph, `<br>` within one.
pub fn paragraphs(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::new();
    for paragraph in text.split("\n\n") {
        let lines: Vec<&str> = paragraph.trim_matches('\n').split('\n').collect();
        if lines.iter().all(|line| line.trim().is_empty()) {
            continue;
        }
        out.push_str("<p>");
        out.push_str(
            &lines
                .iter()
                .map(|line| escape(line))
                .collect::<Vec<_>>()
                .join("<br>"),
        );
        out.push_str("</p>");
    }
    out
}

/// A reply's HTML: the user's text, the attribution line and the quoted original. None when the original is too
/// deep to keep as HTML.
pub fn reply_html(user_text: &str, attribution: &str, original: &Original) -> Option<HtmlPart> {
    let body = clean_body(&original.html)?;
    let html = format!(
        "{}{}<p>{}</p>{QUOTE_OPEN}{body}{QUOTE_CLOSE}",
        document_open(),
        paragraphs(user_text),
        escape(attribution),
    ) + DOCUMENT_CLOSE;
    Some(HtmlPart {
        html,
        inline: used_images(&body, &original.inline),
    })
}

/// A forward's HTML: the user's text, the forwarded message's header block, and the original below it.
pub fn forward_html(
    user_text: &str,
    header_block: &[(&str, String)],
    original: &Original,
) -> Option<HtmlPart> {
    let body = clean_body(&original.html)?;
    let mut block = String::from("<p>---------- Forwarded message ----------");
    for (name, value) in header_block {
        block.push_str(&format!("<br>{name}: {}", escape(value)));
    }
    block.push_str("</p>");
    let html = format!(
        "{}{}{block}<div>{body}</div>{DOCUMENT_CLOSE}",
        document_open(),
        paragraphs(user_text)
    );
    Some(HtmlPart {
        html,
        inline: used_images(&body, &original.inline),
    })
}

fn document_open() -> &'static str {
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>"
}

const DOCUMENT_CLOSE: &str = "</body></html>";

/// The inline images the cleaned HTML still names.
fn used_images(body: &str, inline: &[InlineImage]) -> Vec<InlineImage> {
    let lower = body.to_ascii_lowercase();
    inline
        .iter()
        .filter(|image| lower.contains(&format!("cid:{}", image.cid.to_ascii_lowercase())))
        .cloned()
        .collect()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Elements dropped with everything in them.
const DROPPED: &[&str] = &[
    "script", "iframe", "object", "form", "applet", "frame", "frameset", "noembed", "noframes",
    "title", "template", "svg", "math",
];
/// Elements dropped alone (they have no content).
const DROPPED_VOID: &[&str] = &["base", "meta", "link", "embed"];
/// The document's own structure; its tags go, its content stays.
const STRUCTURE: &[&str] = &["html", "head", "body"];
const VOID: &[&str] = &[
    "area", "br", "col", "hr", "img", "input", "param", "source", "track", "wbr", "keygen",
];

#[derive(Default)]
struct Cleaner {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    styles: String,
    body: String,
    /// Inside `<head>` and not yet in the body: text there is dropped.
    in_head: bool,
    /// Inside a dropped element: its name and how many of it are open.
    dropping: Option<(String, usize)>,
    /// Inside `<style>`: its text goes to `styles` as it is.
    in_style: bool,
    /// Inside a raw-text element whose text is kept as it is (`textarea` is not; `xmp`, `plaintext` are).
    raw: bool,
    /// The open elements of the body, innermost last.
    open: Vec<String>,
    too_deep: bool,
}

impl TokenSink for Cleaner {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut state = self.state.borrow_mut();
        if state.too_deep {
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) => state.tag(tag),
            Token::CharacterTokens(text) => {
                state.text(&text);
                TokenSinkResult::Continue
            }
            _ => TokenSinkResult::Continue,
        }
    }
}

impl State {
    fn tag(&mut self, tag: Tag) -> TokenSinkResult<()> {
        let name = tag.name.to_string();
        let start = tag.kind == TagKind::StartTag;
        if let Some((dropped, open)) = &mut self.dropping {
            if *dropped == name {
                if start {
                    *open += 1;
                } else {
                    *open -= 1;
                    if *open == 0 {
                        self.dropping = None;
                    }
                }
            }
            return raw_kind(&name, start);
        }
        if name == "style" {
            if start {
                self.in_style = true;
                self.styles.push_str(&start_tag(&tag));
                return TokenSinkResult::RawData(RawKind::Rawtext);
            }
            if self.in_style {
                self.in_style = false;
                self.styles.push_str("</style>");
            }
            return TokenSinkResult::Continue;
        }
        if STRUCTURE.contains(&name.as_str()) {
            match (name.as_str(), start) {
                ("head", true) => self.in_head = true,
                ("head", false) | ("body", true) => self.in_head = false,
                _ => {}
            }
            return TokenSinkResult::Continue;
        }
        if DROPPED.contains(&name.as_str()) {
            if start && !tag.self_closing {
                self.dropping = Some((name.clone(), 1));
                return raw_kind(&name, start);
            }
            return TokenSinkResult::Continue;
        }
        if DROPPED_VOID.contains(&name.as_str()) || self.in_head {
            return TokenSinkResult::Continue;
        }
        if start {
            self.body.push_str(&start_tag(&tag));
            if !VOID.contains(&name.as_str()) && !tag.self_closing {
                if self.open.len() >= MAX_DEPTH {
                    self.too_deep = true;
                    return TokenSinkResult::Continue;
                }
                let kind = raw_kind(&name, true);
                self.raw = matches!(
                    kind,
                    TokenSinkResult::RawData(RawKind::Rawtext) | TokenSinkResult::Plaintext
                );
                self.open.push(name);
                return kind;
            }
        } else if let Some(at) = self.open.iter().rposition(|open| *open == name) {
            self.raw = false;
            for closed in self.open.drain(at..).rev() {
                self.body.push_str(&format!("</{closed}>"));
            }
        }
        TokenSinkResult::Continue
    }

    fn text(&mut self, text: &str) {
        if self.dropping.is_some() || self.in_head && !self.in_style {
            return;
        }
        if self.in_style {
            self.styles.push_str(text);
        } else if self.raw {
            self.body.push_str(text);
        } else {
            self.body.push_str(&escape_text(text));
        }
    }
}

/// What the tokenizer reads after a start tag of `name`, as a browser's tree builder tells it.
fn raw_kind(name: &str, start: bool) -> TokenSinkResult<()> {
    if !start {
        return TokenSinkResult::Continue;
    }
    match name {
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "style" | "iframe" | "xmp" | "noembed" | "noframes" => {
            TokenSinkResult::RawData(RawKind::Rawtext)
        }
        "textarea" | "title" => TokenSinkResult::RawData(RawKind::Rcdata),
        "plaintext" => TokenSinkResult::Plaintext,
        _ => TokenSinkResult::Continue,
    }
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\u{a0}', "&nbsp;")
}

/// A start tag as written, without its `on…` attributes and `javascript:` URLs.
fn start_tag(tag: &Tag) -> String {
    let mut out = format!("<{}", tag.name);
    for attr in &tag.attrs {
        let name = attr.name.local.to_ascii_lowercase();
        if name.starts_with("on") || script_url(&attr.value) {
            continue;
        }
        let prefix = match &attr.name.prefix {
            Some(prefix) => format!("{prefix}:"),
            None => String::new(),
        };
        out.push_str(&format!(
            " {prefix}{}=\"{}\"",
            attr.name.local,
            attr.value.replace('&', "&amp;").replace('"', "&quot;")
        ));
    }
    if tag.self_closing {
        out.push_str(" /");
    }
    out.push('>');
    out
}

/// Whether a URL runs script when followed, however its scheme is spaced or cased.
fn script_url(value: &str) -> bool {
    let scheme: String = value
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .take(11)
        .collect::<String>()
        .to_ascii_lowercase();
    scheme.starts_with("javascript:") || scheme.starts_with("vbscript:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original(html: &str) -> Original {
        Original {
            html: html.into(),
            inline: vec![],
        }
    }

    #[test]
    fn user_text_becomes_escaped_paragraphs() {
        assert_eq!(paragraphs("a < b\nc\n\nd"), "<p>a &lt; b<br>c</p><p>d</p>");
        assert_eq!(paragraphs(""), "");
        assert_eq!(paragraphs("x\n\n\n\ny\n"), "<p>x</p><p>y</p>");
    }

    #[test]
    fn the_quote_uses_the_exact_wrapper() {
        let part = reply_html(
            "Thanks!",
            "On Mon, 2 Nov 2026 10:00, Linus wrote:",
            &original("<p>hi</p>"),
        )
        .unwrap();
        let wrapper = r#"<div class="gmail_quote"><blockquote type="cite" class="gmail_quote" style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex">"#;
        assert!(
            part.html
                .contains(&format!("{wrapper}<p>hi</p></blockquote></div>")),
            "{}",
            part.html
        );
        assert!(
            part.html
                .contains("<p>Thanks!</p><p>On Mon, 2 Nov 2026 10:00, Linus wrote:</p>")
        );
    }

    #[test]
    fn each_dangerous_element_and_attribute_is_removed() {
        let cases = [
            (
                "<p>a</p><script>alert(1)</script><p>b</p>",
                "<p>a</p><p>b</p>",
            ),
            ("<iframe src=\"https://x\">fallback</iframe>ok", "ok"),
            ("<object data=\"x.swf\"><p>alt</p></object>ok", "ok"),
            ("<embed src=\"x.swf\">ok", "ok"),
            (
                "<form action=\"https://x\"><input name=\"q\"></form>ok",
                "ok",
            ),
            (
                "<head><meta http-equiv=\"refresh\" content=\"0;url=https://x\"></head><body>ok</body>",
                "ok",
            ),
            (
                "<head><base href=\"https://evil.example/\"></head><body><a href=\"page\">x</a></body>",
                "<a href=\"page\">x</a>",
            ),
            (
                "<img src=\"cid:a@b\" onerror=\"alert(1)\" alt=\"logo\">",
                "<img src=\"cid:a@b\" alt=\"logo\">",
            ),
            ("<a href=\" JaVaScRiPt:alert(1)\">x</a>", "<a>x</a>"),
            ("<a href=\"java\tscript:alert(1)\">x</a>", "<a>x</a>"),
            (
                "<div ONCLICK=\"x\" class=\"c\">y</div>",
                "<div class=\"c\">y</div>",
            ),
            ("<form action=\"javascript:x\"></form>z", "z"),
        ];
        for (input, expected) in cases {
            assert_eq!(clean_body(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn the_original_cannot_close_the_quote() {
        let part = reply_html(
            "",
            "On x, y wrote:",
            &original("<div>a</blockquote></div></div><p>b"),
        )
        .unwrap();
        assert!(
            part.html
                .ends_with(&format!("<div>a</div><p>b</p>{QUOTE_CLOSE}</body></html>")),
            "{}",
            part.html
        );
    }

    #[test]
    fn styles_are_kept_in_front_of_the_body() {
        let html = "<html><head><title>T</title><style>p { color: red; } a > b {}</style></head><body><p>x</p><style>.y{}</style></body></html>";
        assert_eq!(
            clean_body(html).unwrap(),
            "<style>p { color: red; } a > b {}</style><style>.y{}</style><p>x</p>"
        );
    }

    #[test]
    fn remote_images_are_left_as_they_are() {
        let html = "<img src=\"https://tracker.example/p.gif?u=1&amp;v=2\" width=\"1\">";
        assert_eq!(
            clean_body(html).unwrap(),
            "<img src=\"https://tracker.example/p.gif?u=1&amp;v=2\" width=\"1\">"
        );
    }

    #[test]
    fn cid_parts_travel_with_their_content_ids() {
        let image = |cid: &str| InlineImage {
            cid: cid.into(),
            content_type: "image/png".into(),
            bytes: vec![1, 2],
        };
        let original = Original {
            html: "<base href=\"https://x/\"><img src=\"cid:Logo@x\" onerror=\"y\">".into(),
            inline: vec![image("logo@x"), image("unused@x")],
        };
        let part = reply_html("", "On x, y wrote:", &original).unwrap();
        assert_eq!(part.inline, [image("logo@x")]);
        assert!(part.html.contains("<img src=\"cid:Logo@x\">"));
        assert!(!part.html.contains("base"));
    }

    #[test]
    fn markup_nested_too_deep_is_given_up() {
        let deep = "<div>".repeat(MAX_DEPTH + 1);
        assert_eq!(clean_body(&deep), None);
        let fine = "<div>".repeat(10);
        assert_eq!(
            clean_body(&fine).unwrap(),
            format!("{fine}{}", "</div>".repeat(10))
        );
    }

    #[test]
    fn a_forward_puts_its_header_block_above_the_original() {
        let part = forward_html(
            "FYI",
            &[("From", "Linus <l@x.org>".into()), ("Subject", "s".into())],
            &original("<p>hi</p>"),
        )
        .unwrap();
        assert!(part.html.contains(
            "<p>FYI</p><p>---------- Forwarded message ----------<br>From: Linus &lt;l@x.org&gt;<br>Subject: s</p><div><p>hi</p></div>"
        ), "{}", part.html);
        assert!(!part.html.contains("blockquote"));
    }
}
