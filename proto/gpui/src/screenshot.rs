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

/// p50, p95 and max of `times`, in milliseconds; the same report as `src/gui/bench.rs`.
fn report(name: &str, times: &mut [Duration]) {
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    println!(
        "{name}: {} frames, p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        times.len(),
        ms(at(0.5)),
        ms(at(0.95)),
        ms(at(1.0))
    );
}

/// The GPUI side of `src/gui/bench.rs`: holding `j` on 20 000 messages, each frame drawn offscreen and read back.
#[test]
#[ignore = "benchmark, run by hand: cargo test --release hold_j -- --ignored --nocapture"]
fn hold_j_on_20000_messages() {
    let text = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "Hanken Grotesk",
    ));
    let mut cx = HeadlessAppContext::with_platform(
        text,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    cx.update(crate::init);
    let dir = tempfile::tempdir().unwrap();
    let (home, shown) = crate::open_home(dir.path(), Some(20_000)).unwrap();
    let (model, _commands) = crate::open_model(&home, shown).unwrap();
    let window = cx
        .open_window(size(px(1280.), px(800.)), |window, cx| {
            let mail = Mail::new(model, window, cx);
            cx.new(|_| mail)
        })
        .unwrap();
    cx.run_until_parked();
    cx.capture_screenshot(window.into()).unwrap();
    let mut times = Vec::new();
    for _ in 0..300 {
        let start = std::time::Instant::now();
        window
            .update(&mut cx, |_, window, cx| {
                window.dispatch_action(Box::new(crate::list::Next), cx)
            })
            .unwrap();
        cx.run_until_parked();
        cx.capture_screenshot(window.into()).unwrap();
        times.push(start.elapsed());
    }
    let cursor = window
        .update(&mut cx, |mail, _, cx| mail.model.read(cx).cursor)
        .unwrap();
    assert_eq!(cursor, 300);
    report(
        "GPUI, key to frame read back, 1280×800 at scale 2",
        &mut times,
    );
}
