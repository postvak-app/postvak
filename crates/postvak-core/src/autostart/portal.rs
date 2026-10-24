//! The Background portal (`org.freedesktop.portal.Background`), which a Flatpak asks to start it at login; the desktop
//! writes the host's autostart entry, and may ask the user first.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// Makes each request's token, and so its answer's path, this process's own.
static REQUESTS: AtomicU32 = AtomicU32::new(0);

/// Asks the desktop to start Postvak at login, or no longer; whether it now does.
pub(super) fn request(on: bool) -> Result<bool> {
    let bus = Connection::session().context("no session bus for the Background portal")?;
    let sender = bus
        .unique_name()
        .context("the session bus gave no name")?
        .to_string();
    let token = format!(
        "postvak{}_{}",
        std::process::id(),
        REQUESTS.fetch_add(1, Ordering::Relaxed)
    );
    // Listening before the call, on the path the portal will answer on, is the portal's way to never miss the answer.
    let request = Proxy::new(
        &bus,
        DESTINATION,
        request_path(&sender, &token),
        "org.freedesktop.portal.Request",
    )?;
    let mut answers = request.receive_signal("Response")?;
    let commandline: Vec<String> = std::iter::once("postvak")
        .chain(super::GUI_ARGS)
        .map(String::from)
        .collect();
    let options: HashMap<&str, Value> = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        (
            "reason",
            Value::from("Postvak opens in the tray at login to sync your mail"),
        ),
        ("autostart", Value::from(on)),
        ("commandline", Value::from(commandline)),
    ]);
    bus.call_method(
        Some(DESTINATION),
        PATH,
        Some("org.freedesktop.portal.Background"),
        "RequestBackground",
        &("", options),
    )
    .context("asking the Background portal")?;
    let answer = answers
        .next()
        .context("the Background portal did not answer")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = answer.body().deserialize()?;
    granted(code, &results)
}

/// The path a portal request with `token` answers on: the sender's unique name without its `:`, dots as underscores.
fn request_path(sender: &str, token: &str) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    format!("{PATH}/request/{sender}/{token}")
}

/// Whether the desktop now starts Postvak at login, from the portal's response.
fn granted(code: u32, results: &HashMap<String, OwnedValue>) -> Result<bool> {
    match code {
        0 => Ok(results
            .get("autostart")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false)),
        1 => bail!("cancelled"),
        _ => bail!("the Background portal refused the request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_comes_on_the_senders_request_path() {
        assert_eq!(
            request_path(":1.42", "postvak7"),
            "/org/freedesktop/portal/desktop/request/1_42/postvak7"
        );
    }

    #[test]
    fn the_desktop_grants_autostart_or_says_why_not() {
        let results = HashMap::from([("autostart".to_string(), OwnedValue::from(true))]);
        assert!(granted(0, &results).unwrap());
        assert!(!granted(0, &HashMap::new()).unwrap());
        assert!(granted(1, &results).is_err());
        assert!(granted(2, &results).is_err());
    }
}
