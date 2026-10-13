//! Throwaway GPUI prototype of the Postvak mail window; never merged. See README.md.

use std::borrow::Cow;

use gpui::{App, Context, IntoElement, Render, Window, div, prelude::*, px, rgb, size};

/// The fonts the egui GUI bundles, so both builds draw the same glyphs.
const FONTS: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/HankenGrotesk-Medium.ttf"),
    include_bytes!("../../../assets/fonts/HankenGrotesk-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Newsreader-Medium.ttf"),
    include_bytes!("../../../assets/fonts/SplineSansMono-Medium.ttf"),
];

struct Root;

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0xfbf8f3))
            .text_color(rgb(0x3b1418))
            .font_family("Newsreader")
            .text_size(px(32.))
            .child("Postvak")
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let fonts = FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
        if let Err(e) = cx.text_system().add_fonts(fonts) {
            eprintln!("could not load the bundled fonts: {e}");
        }
        let bounds = gpui::Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let options = gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            app_id: Some("io.github.postvak_app.postvak-gpui-proto".into()),
            ..Default::default()
        };
        if let Err(e) = cx.open_window(options, |_, cx| cx.new(|_| Root)) {
            eprintln!("could not open the window: {e}");
            cx.quit();
        }
    });
}
