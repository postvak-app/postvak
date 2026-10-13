//! The window drawn offscreen and read back, as the egui snapshots are under lavapipe: GPUI's `HeadlessAppContext`
//! with the platform's headless renderer (wgpu on Linux). Writes `target/screenshots/inbox.png`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, HeadlessAppContext, px, size};

use crate::{Mail, test_model};

/// The snapshot inbox with the newsletter open, once its first strip is painted.
fn screenshot() -> image::RgbaImage {
    // Only the bundled fonts, so a machine's own fonts cannot change the picture.
    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "Hanken Grotesk",
    ));
    let mut cx = HeadlessAppContext::with_platform(
        text,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    // The render thread is a real thread.
    cx.allow_parking();
    cx.update(crate::init);
    let (_dir, model, _commands) = test_model(true);
    let window = cx
        .open_window(size(px(1280.), px(800.)), |window, cx| {
            let mail = Mail::new(model, window, cx);
            cx.new(|_| mail)
        })
        .unwrap();
    // Each capture draws a frame; the page is laid out after the first one gives the body its width.
    for _ in 0..2000 {
        cx.run_until_parked();
        let shot = cx
            .capture_screenshot(window.into())
            .expect("headless renderer");
        let painted = window
            .update(&mut cx, |mail, _, cx| mail.body.read(cx).painted_strips())
            .unwrap();
        if painted > 0 {
            cx.run_until_parked();
            return cx.capture_screenshot(window.into()).unwrap_or(shot);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no strip painted within 10 s");
}

#[test]
fn the_window_renders_offscreen_and_the_same_twice() {
    let first = screenshot();
    let second = screenshot();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    first.save(dir.join("inbox.png")).unwrap();
    assert!(
        first.width() >= 1280 && first.height() >= 800,
        "{}×{}",
        first.width(),
        first.height()
    );
    let colours: std::collections::HashSet<_> = first.pixels().map(|p| p.0).collect();
    assert!(
        colours.len() > 50,
        "only {} colours: nothing drawn?",
        colours.len()
    );
    if first != second {
        second.save(dir.join("inbox-second.png")).unwrap();
        let differ = first
            .pixels()
            .zip(second.pixels())
            .filter(|(a, b)| a != b)
            .count();
        panic!("two renders differ in {differ} pixels; see target/screenshots");
    }
}
