//! Throwaway GPUI prototype of the Postvak mail window; never merged. See README.md.
//!
//! `POSTVAK_PROTO_COUNT=20000` opens a generated INBOX of that many messages instead of the snapshot mailbox.
mod body;
mod list;
mod model;
#[cfg(all(test, target_os = "linux"))]
mod screenshot;
mod theme;

use std::borrow::Cow;
use std::path::Path;
use std::sync::mpsc::Receiver;

use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit::component::Theme;
use postvak::daemon::Client;
use postvak::gui::fixture::{self, Home};
use postvak::sync::Command;

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
fn add_newsletter(home: &Home) -> anyhow::Result<()> {
    let text = "This week in the reading room";
    let mut m = fixture::mail(97, "The Weekly Reader <news@example.org>", text, "", text);
    m.date = Some(fixture::AT);
    m.internaldate = fixture::AT;
    home.add("work", m)?;
    home.store("work")?
        .set_raw("INBOX", 97, newsletter_raw().as_bytes(), text)?;
    Ok(())
}

/// The snapshot mailbox's `work` INBOX with the newsletter, or with `count` a generated one, in a new home under
/// `dir`; and the indices of the accounts the list shows.
fn open_home(dir: &Path, count: Option<u32>) -> anyhow::Result<(Home, Vec<usize>)> {
    // A home made by an earlier run with `--home`: open it as it is.
    let paths = postvak::paths::Paths::under(dir);
    if paths.config_file().exists() {
        let config = postvak::config::Config::load(&paths.config_file())?;
        let accounts: Vec<String> = config.accounts.into_iter().map(|a| a.name).collect();
        let shown = vec![accounts.iter().position(|a| a == "work").unwrap_or(0)];
        return Ok((Home { accounts, paths }, shown));
    }
    Ok(match count {
        Some(count) => (fixture::generated(dir, count)?, vec![0]),
        None => {
            let home = fixture::mailbox(dir, "light")?;
            add_newsletter(&home)?;
            (home, vec![1])
        }
    })
}

/// A model over a client with no daemon behind it, and the receiver of what it sends; dropping the receiver makes
/// every send fail.
fn open_model(
    home: &Home,
    shown: Vec<usize>,
) -> anyhow::Result<(Model, Receiver<(String, Command)>)> {
    let names: Vec<&str> = home.accounts.iter().map(String::as_str).collect();
    let (client, commands, _) = Client::in_memory(&names);
    Ok((Model::open(home, shown, client)?, commands))
}

/// The snapshot inbox, with or without the newsletter, for tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub fn test_model(newsletter: bool) -> (tempfile::TempDir, Model, Receiver<(String, Command)>) {
    let dir = tempfile::tempdir().unwrap();
    let home = fixture::mailbox(dir.path(), "light").unwrap();
    if newsletter {
        add_newsletter(&home).unwrap();
    }
    let (model, commands) = open_model(&home, vec![1]).unwrap();
    (dir, model, commands)
}

/// The window: the list, then the body.
pub struct Mail {
    #[cfg(test)]
    pub model: Entity<Model>,
    pub list: Entity<MessageList>,
    pub body: Entity<BodyPane>,
}

impl Mail {
    /// The list over `model`, focused, and the body that follows its cursor.
    pub fn new(model: Model, window: &mut Window, cx: &mut App) -> Mail {
        let model = cx.new(|_| model);
        let list = cx.new(|cx| MessageList::new(model.clone(), window, cx));
        MessageList::focus(&list, window, cx);
        let body = cx.new(|cx| BodyPane::new(model.clone(), cx));
        Mail {
            #[cfg(test)]
            model,
            list,
            body,
        }
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

/// GPUI Kit with its list in Postvak's light colours, the keys, and the bundled fonts.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    let kit = Theme::global_mut(cx);
    kit.list_active = theme::selected();
    kit.list_active_border = theme::selected();
    kit.list_hover = theme::hover();
    kit.tokens.list_hover = theme::hover().into();
    cx.bind_keys(list::bindings());
    let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("could not load the bundled fonts: {e}");
    }
}

fn main() -> anyhow::Result<()> {
    // `--home DIR` makes the home in DIR on first use and keeps it, so the egui build can open the same mailbox;
    // without it the home is a temp dir. Both outlive the window, as does the receiver that lets sends succeed.
    let args: Vec<String> = std::env::args().collect();
    let kept = match args.as_slice() {
        [_, flag, dir] if flag == "--home" => Some(std::path::PathBuf::from(dir)),
        [_] => None,
        _ => anyhow::bail!("usage: postvak-gpui-proto [--home DIR]"),
    };
    let temp = tempfile::tempdir()?;
    let dir = kept.as_deref().unwrap_or(temp.path());
    std::fs::create_dir_all(dir)?;
    let count = std::env::var("POSTVAK_PROTO_COUNT")
        .ok()
        .and_then(|n| n.parse().ok());
    let (home, shown) = open_home(dir, count)?;
    let (model, _commands) = open_model(&home, shown)?;
    gpui_kit::application().run(move |cx: &mut App| {
        init(cx);
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
