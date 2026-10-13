//! Throwaway GPUI prototype of the Postvak mail window; never merged. See README.md.
//!
//! `POSTVAK_PROTO_COUNT=20000` opens a generated INBOX of that many messages instead of the snapshot mailbox.
mod body;
mod list;
mod model;
mod theme;

use std::borrow::Cow;

use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit::component::Theme;
use postvak::gui::fixture::{self, Home};

use body::BodyPane;
use list::MessageList;
use model::Model;

/// The fonts the egui GUI bundles, so both builds draw the same glyphs.
const FONTS: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/HankenGrotesk-Medium.ttf"),
    include_bytes!("../../../assets/fonts/HankenGrotesk-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Newsreader-Medium.ttf"),
    include_bytes!("../../../assets/fonts/SplineSansMono-Medium.ttf"),
];

/// `tests/fixtures/newsletter.html`, the HTML the egui snapshots render.
const NEWSLETTER: &str = include_str!("../../../tests/fixtures/newsletter.html");

/// A 1×1 PNG for the newsletter's `cid:` image.
const DOT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// The newsletter as a raw message: its HTML with a `cid:dot@example.com` image beside it.
fn newsletter_raw() -> String {
    let parts = [
        "From: news@example.org",
        "Subject: news",
        "MIME-Version: 1.0",
        "Content-Type: multipart/related; boundary=\"b\"",
        "",
        "--b",
        "Content-Type: text/html; charset=utf-8",
        "",
        NEWSLETTER,
        "--b",
        "Content-Type: image/png",
        "Content-ID: <dot@example.com>",
        "Content-Transfer-Encoding: base64",
        "",
        DOT,
        "--b--",
        "",
    ];
    parts.join("\r\n")
}

/// Adds the newsletter to work's INBOX as its newest message, unread.
pub fn add_newsletter(home: &Home) -> anyhow::Result<()> {
    let text = "This week in the reading room";
    let mut m = fixture::mail(97, "The Weekly Reader <news@example.org>", text, "", text);
    m.date = Some(fixture::AT);
    m.internaldate = fixture::AT;
    home.add("work", m)?;
    home.store("work")?
        .set_raw("INBOX", 97, newsletter_raw().as_bytes(), text)?;
    Ok(())
}

/// The snapshot mailbox's `work` INBOX with the newsletter, or a generated one; in a temp home that lives as long as
/// the process.
fn open_fixture() -> anyhow::Result<Model> {
    let dir = Box::leak(Box::new(tempfile::tempdir()?));
    match std::env::var("POSTVAK_PROTO_COUNT")
        .ok()
        .and_then(|n| n.parse().ok())
    {
        Some(count) => Model::open(&fixture::generated(dir.path(), count)?, vec![0]),
        None => {
            let home = fixture::mailbox(dir.path(), "light")?;
            add_newsletter(&home)?;
            Model::open(&home, vec![1])
        }
    }
}

/// The window: the list, then the body.
pub struct Mail {
    pub list: Entity<MessageList>,
    pub body: Entity<BodyPane>,
}

impl Mail {
    /// The list over `model`, focused, and the body that follows its cursor.
    pub fn new(model: Model, window: &mut Window, cx: &mut App) -> Mail {
        let list = cx.new(|cx| MessageList::new(model, window, cx));
        let rows = list.read(cx).list.clone();
        rows.update(cx, |rows, cx| rows.focus(window, cx));
        let body = cx.new(|cx| BodyPane::new(&list, cx));
        Mail { list, body }
    }
}

impl Render for Mail {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .child(
                div()
                    .w(theme::LIST_WIDTH)
                    .h_full()
                    .border_r_1()
                    .border_color(theme::border())
                    .child(self.list.clone()),
            )
            .child(div().flex_1().h_full().child(self.body.clone()))
    }
}

/// GPUI Kit's list colours as Postvak's light panes.
fn style(cx: &mut App) {
    let kit = Theme::global_mut(cx);
    kit.list_active = theme::selected();
    kit.list_active_border = theme::selected();
    kit.list_hover = theme::hover();
    kit.tokens.list_hover = theme::hover().into();
}

fn main() -> anyhow::Result<()> {
    let model = open_fixture()?;
    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        style(cx);
        cx.bind_keys(list::bindings());
        let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
        if let Err(e) = cx.text_system().add_fonts(fonts) {
            eprintln!("could not load the bundled fonts: {e}");
        }
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("io.github.postvak_app.postvak-gpui-proto".into()),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            let mail = Mail::new(model, window, cx);
            cx.new(|_| mail)
        });
        if let Err(e) = opened {
            eprintln!("could not open the window: {e}");
            cx.quit();
        }
    });
    Ok(())
}
