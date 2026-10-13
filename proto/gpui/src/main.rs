//! Throwaway GPUI prototype of the Postvak mail window; never merged. See README.md.
//!
//! `POSTVAK_PROTO_COUNT=20000` opens a generated INBOX of that many messages instead of the snapshot mailbox.
mod list;
mod model;
mod theme;

use std::borrow::Cow;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_kit::component::Theme;

use list::MessageList;
use model::Model;

/// The fonts the egui GUI bundles, so both builds draw the same glyphs.
const FONTS: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/HankenGrotesk-Medium.ttf"),
    include_bytes!("../../../assets/fonts/HankenGrotesk-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Newsreader-Medium.ttf"),
    include_bytes!("../../../assets/fonts/SplineSansMono-Medium.ttf"),
];

/// The snapshot mailbox's `work` INBOX, or a generated one; in a temp home that lives as long as the process.
fn fixture() -> anyhow::Result<Model> {
    let dir = Box::leak(Box::new(tempfile::tempdir()?));
    match std::env::var("POSTVAK_PROTO_COUNT")
        .ok()
        .and_then(|n| n.parse().ok())
    {
        Some(count) => Model::open(
            &postvak::gui::fixture::generated(dir.path(), count)?,
            vec![0],
        ),
        None => Model::open(
            &postvak::gui::fixture::mailbox(dir.path(), "light")?,
            vec![1],
        ),
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
    let model = fixture()?;
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
            let list = cx.new(|cx| MessageList::new(model, window, cx));
            let rows = list.read(cx).list.clone();
            rows.update(cx, |rows, cx| rows.focus(window, cx));
            list
        });
        if let Err(e) = opened {
            eprintln!("could not open the window: {e}");
            cx.quit();
        }
    });
    Ok(())
}
