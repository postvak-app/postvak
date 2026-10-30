//! Whether the desktop asks apps for less motion. On macOS that is Reduce motion (Accessibility → Display); on Linux
//! the Settings portal's `reduced-motion`, GNOME's Animations switch, or Plasma's animation speed set to Instant.

#[cfg(target_os = "linux")]
use crate::portal::Settings;
#[cfg(target_os = "linux")]
use zbus::zvariant::Value;

#[cfg(target_os = "linux")]
const APPEARANCE: &str = "org.freedesktop.appearance";
#[cfg(target_os = "linux")]
const GNOME: &str = "org.gnome.desktop.interface";
#[cfg(target_os = "linux")]
const PLASMA: &str = "org.kde.kdeglobals.KDE";

/// Calls `changed` with whether the desktop asks for less motion now, then again each time that may have changed.
/// Blocks while the session bus lasts, so on a thread of its own; fails without a session bus or a Settings portal.
#[cfg(target_os = "linux")]
pub fn follow(mut changed: impl FnMut(bool)) -> anyhow::Result<()> {
    crate::portal::follow_settings(&[APPEARANCE, GNOME, PLASMA], |settings| {
        changed(reduced(settings));
    })
}

/// Whether the desktop asks for less motion now. Read on each change of the window's focus, since a change in System
/// Settings comes with leaving the window and coming back.
#[cfg(target_os = "macos")]
pub fn reduced() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Whether any of the settings asks for less motion. The portal's own key is the standard, but a desktop's portal may
/// not have it yet, so GNOME's and Plasma's own settings count too.
#[cfg(target_os = "linux")]
fn reduced(settings: &Settings) -> bool {
    settings.iter().any(|(namespace, keys)| {
        keys.iter()
            .any(|(key, value)| asks_less(namespace, key, value))
    })
}

#[cfg(target_os = "linux")]
fn asks_less(namespace: &str, key: &str, value: &Value) -> bool {
    match (namespace, key, value) {
        // Some portals wrap a value in another variant.
        (_, _, Value::Value(inner)) => asks_less(namespace, key, inner),
        // 0 no preference, 1 reduced.
        (APPEARANCE, "reduced-motion", Value::U32(preference)) => *preference == 1,
        (GNOME, "enable-animations", Value::Bool(enabled)) => !enabled,
        // 1 is the normal speed; Instant, the slider's end, is 0. The Plasma portal gives kdeglobals entries as text.
        (PLASMA, "AnimationDurationFactor", Value::F64(factor)) => *factor <= 0.0,
        (PLASMA, "AnimationDurationFactor", Value::Str(factor)) => factor
            .trim()
            .parse::<f64>()
            .is_ok_and(|factor| factor <= 0.0),
        _ => false,
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::collections::HashMap;

    use zbus::zvariant::OwnedValue;

    use super::*;

    fn settings(namespace: &str, key: &str, value: Value<'static>) -> Settings {
        let value = OwnedValue::try_from(value).unwrap();
        HashMap::from([(
            namespace.to_string(),
            HashMap::from([(key.to_string(), value)]),
        )])
    }

    #[test]
    fn the_portals_reduced_motion_asks_for_less() {
        assert!(reduced(&settings(
            APPEARANCE,
            "reduced-motion",
            Value::U32(1)
        )));
        assert!(!reduced(&settings(
            APPEARANCE,
            "reduced-motion",
            Value::U32(0)
        )));
    }

    #[test]
    fn gnome_with_animations_off_asks_for_less() {
        assert!(reduced(&settings(
            GNOME,
            "enable-animations",
            Value::Bool(false)
        )));
        assert!(!reduced(&settings(
            GNOME,
            "enable-animations",
            Value::Bool(true)
        )));
    }

    #[test]
    fn plasma_at_instant_speed_asks_for_less_as_a_number_or_as_text() {
        assert!(reduced(&settings(
            PLASMA,
            "AnimationDurationFactor",
            Value::F64(0.0)
        )));
        assert!(reduced(&settings(
            PLASMA,
            "AnimationDurationFactor",
            Value::from("0")
        )));
        assert!(!reduced(&settings(
            PLASMA,
            "AnimationDurationFactor",
            Value::F64(0.5)
        )));
        assert!(!reduced(&settings(
            PLASMA,
            "AnimationDurationFactor",
            Value::from("1")
        )));
        assert!(!reduced(&settings(
            PLASMA,
            "AnimationDurationFactor",
            Value::from("fast")
        )));
    }

    #[test]
    fn a_value_wrapped_in_another_variant_counts() {
        let wrapped = Value::Value(Box::new(Value::U32(1)));
        assert!(reduced(&settings(APPEARANCE, "reduced-motion", wrapped)));
    }

    #[test]
    fn other_settings_and_none_ask_for_nothing() {
        assert!(!reduced(&HashMap::new()));
        assert!(!reduced(&settings(
            APPEARANCE,
            "color-scheme",
            Value::U32(1)
        )));
        assert!(!reduced(&settings(
            GNOME,
            "enable-animations",
            Value::U32(0)
        )));
    }
}
