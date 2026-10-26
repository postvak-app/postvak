//! Counts grouped the way the device's region writes numbers.
use std::sync::OnceLock;

/// The thousands separator for a BCP 47 or POSIX locale name; None when unknown, which shows plain digits.
pub(crate) fn separator_for(locale: &str) -> Option<char> {
    let language = locale
        .split(['-', '_', '.'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    // ponytail: language-only table; region overrides such as de-CH (apostrophe) or Indian grouping when someone asks
    match language.as_str() {
        "da" | "de" | "el" | "es" | "id" | "it" | "nl" | "pt" | "tr" => Some('.'),
        "en" | "he" | "ja" | "ko" | "th" | "zh" => Some(','),
        "cs" | "fi" | "fr" | "nb" | "pl" | "ru" | "sk" | "sv" | "uk" => Some('\u{202f}'),
        _ => None,
    }
}

/// `n` grouped for this device's locale, read once.
pub(crate) fn count(n: u64) -> String {
    static SEPARATOR: OnceLock<Option<char>> = OnceLock::new();
    let separator =
        *SEPARATOR.get_or_init(|| sys_locale::get_locale().as_deref().and_then(separator_for));
    separator.map_or_else(
        || n.to_string(),
        |separator| postvak_core::sync::group_digits(n, separator),
    )
}

/// `n` grouped, then the noun in the singular for one and the plural otherwise: "1 message", "12,345 messages".
pub(crate) fn counted(n: u64, singular: &str, plural: &str) -> String {
    format!("{} {}", count(n), noun(n, singular, plural))
}

/// The noun alone, for a sentence that puts a word between the number and it: "3 cached messages".
pub(crate) fn noun<'a>(n: u64, singular: &'a str, plural: &'a str) -> &'a str {
    if n == 1 { singular } else { plural }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_picks_the_singular_only_for_one() {
        assert_eq!(counted(0, "message", "messages"), "0 messages");
        assert_eq!(counted(1, "message", "messages"), "1 message");
        assert_eq!(counted(2, "message", "messages"), "2 messages");
    }

    #[test]
    fn locales_pick_their_separator_and_unknown_ones_none() {
        assert_eq!(separator_for("nl-NL"), Some('.'));
        assert_eq!(separator_for("nl_NL.UTF-8"), Some('.'));
        assert_eq!(separator_for("de"), Some('.'));
        assert_eq!(separator_for("en-US"), Some(','));
        assert_eq!(separator_for("fr-FR"), Some('\u{202f}'));
        assert_eq!(separator_for("C"), None);
        assert_eq!(separator_for("POSIX"), None);
        assert_eq!(separator_for(""), None);
        assert_eq!(separator_for("xx"), None);
    }
}
