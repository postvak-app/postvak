//! The Background portal (`org.freedesktop.portal.Background`), which a Flatpak asks to start it at login; the desktop
//! writes the host's autostart entry, and may ask the user first.
use std::collections::HashMap;

use anyhow::{Result, bail};
use zbus::zvariant::{OwnedValue, Value};

/// Asks the desktop to start Postvak at login, or no longer; whether it now does.
pub(super) fn request(on: bool) -> Result<bool> {
    let commandline: Vec<String> = std::iter::once("postvak")
        .chain(super::GUI_ARGS)
        .map(String::from)
        .collect();
    let answer = crate::portal::request("Background", "RequestBackground", |token| {
        let options: HashMap<&str, Value> = HashMap::from([
            ("handle_token", Value::from(token.to_string())),
            (
                "reason",
                Value::from("Postvak opens in the tray at login to sync your mail"),
            ),
            ("autostart", Value::from(on)),
            ("commandline", Value::from(commandline)),
        ]);
        ("", options)
    })?;
    let Some(results) = answer else {
        bail!("cancelled");
    };
    Ok(granted(&results))
}

/// Whether the desktop now starts Postvak at login, from the portal's results.
fn granted(results: &HashMap<String, OwnedValue>) -> bool {
    results
        .get("autostart")
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_grants_autostart_or_not() {
        let results = HashMap::from([("autostart".to_string(), OwnedValue::from(true))]);
        assert!(granted(&results));
        assert!(!granted(&HashMap::new()));
    }
}
