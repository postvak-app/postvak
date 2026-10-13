//! Throwaway Slint prototype of the Postvak mail window; never merged. See README.md.
//!
//! `POSTVAK_PROTO_COUNT=20000` opens a generated INBOX of that many messages instead of the snapshot mailbox;
//! `--home DIR` makes the home in DIR on first use and keeps it, so other builds can open the same mailbox.
mod app;
mod fixture;
mod model;
#[cfg(test)]
mod tests;

/// The code Slint generates from `ui/mail.slint`; it unwraps internally, which this crate's lints deny elsewhere.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::all)]
mod ui {
    slint::include_modules!();
}
pub use ui::*;

use slint::ComponentHandle;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let kept = match args.as_slice() {
        [_, flag, dir] if flag == "--home" => Some(std::path::PathBuf::from(dir)),
        [_] => None,
        _ => anyhow::bail!("usage: postvak-slint-proto [--home DIR]"),
    };
    // Both outlive the window: the home's stores, and the receiver that lets sends succeed.
    let temp = tempfile::tempdir()?;
    let dir = kept.as_deref().unwrap_or(temp.path());
    std::fs::create_dir_all(dir)?;
    let count = std::env::var("POSTVAK_PROTO_COUNT")
        .ok()
        .and_then(|n| n.parse().ok());
    let (home, shown) = fixture::open_home(dir, count)?;
    let (model, _commands) = fixture::open_model(&home, shown)?;
    let ui = MailWindow::new()?;
    let app = app::App::new(&ui, model);
    let _bench = std::env::var_os("POSTVAK_PROTO_BENCH").map(|_| bench(&ui, app.clone()));
    ui.run()?;
    Ok(())
}

/// `POSTVAK_PROTO_BENCH=1`: holds `j` on screen, 300 presses at 30 per second, then prints how long each press took
/// to handle and each frame took to render (femtovg on OpenGL by default), and quits.
fn bench(ui: &MailWindow, app: std::rc::Rc<std::cell::RefCell<app::App>>) -> slint::Timer {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    let frames: Rc<RefCell<(Option<Instant>, Vec<Duration>)>> = Rc::default();
    let rendered = frames.clone();
    let notified = ui.window().set_rendering_notifier(move |state, _| {
        let mut frames = rendered.borrow_mut();
        match state {
            slint::RenderingState::BeforeRendering => frames.0 = Some(Instant::now()),
            slint::RenderingState::AfterRendering => {
                if let Some(start) = frames.0.take() {
                    frames.1.push(start.elapsed());
                }
            }
            _ => {}
        }
    });
    if let Err(e) = notified {
        eprintln!("no rendering notifier: {e}");
    }
    let keys: Rc<RefCell<Vec<Duration>>> = Rc::default();
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            let mut keys = keys.borrow_mut();
            if keys.len() == 300 {
                let report = |name: &str, times: &mut Vec<Duration>| {
                    times.sort();
                    let at = |q: f64| {
                        times[((times.len() - 1) as f64 * q) as usize].as_secs_f64() * 1000.0
                    };
                    println!(
                        "{name}: {} samples, p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
                        times.len(),
                        at(0.5),
                        at(0.95),
                        at(1.0)
                    );
                };
                report("Slint on screen, key handled", &mut keys);
                report(
                    "Slint on screen, frame rendered",
                    &mut frames.borrow_mut().1,
                );
                let _ = slint::quit_event_loop();
                keys.push(Duration::ZERO);
                return;
            }
            if keys.len() > 300 {
                return;
            }
            let start = Instant::now();
            app.borrow_mut().key("j");
            keys.push(start.elapsed());
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        },
    );
    timer
}
