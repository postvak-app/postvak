//! Throwaway Slint prototype of the Postvak mail window; never merged. See README.md.
//!
//! `POSTVAK_PROTO_COUNT=20000` opens a generated INBOX of that many messages instead of the snapshot mailbox;
//! `--home DIR` makes the home in DIR on first use and keeps it, so other builds can open the same mailbox.
mod app;
mod fixture;
mod model;
#[cfg(test)]
mod tests;

slint::include_modules!();

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
    let _app = app::App::new(&ui, model);
    ui.run()?;
    Ok(())
}
