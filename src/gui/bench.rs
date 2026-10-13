//! Holding `j` on a 20 000-message INBOX, each frame drawn offscreen and read back: the egui side of the GPUI
//! prototype's frame-time comparison (`proto/gpui`, plan Task 7). Ignored; run by hand with
//! `cargo test --release --all-features gui::bench -- --ignored --nocapture`.
use std::time::{Duration, Instant};

use eframe::egui;

use super::app::View;
use super::test_support::Fixture;

/// p50, p95 and max of `times`, in milliseconds.
pub(crate) fn report(name: &str, times: &mut [Duration]) {
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

#[test]
#[ignore = "benchmark, run by hand for the GPUI comparison"]
fn hold_j_on_20000_messages() {
    let fx = Fixture::generated(20_000);
    let (mut harness, _wires) = fx.harness();
    harness.set_pixels_per_point(2.0);
    harness.state_mut().select_view(View::Folder {
        account: 0,
        folder: "INBOX".into(),
    });
    harness.run();
    harness.render().unwrap();
    let mut times = Vec::new();
    for _ in 0..300 {
        let start = Instant::now();
        harness.key_press(egui::Key::ArrowDown);
        harness.step();
        harness.render().unwrap();
        times.push(start.elapsed());
    }
    assert_eq!(harness.state().list.cursor, 300);
    report(
        "egui, key to frame read back, 1280×800 at scale 2",
        &mut times,
    );
}
