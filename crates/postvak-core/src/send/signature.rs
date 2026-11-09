//! The signature under the text of a message: `-- ` on a line of its own, then the signature.

/// `text` with the signature below it, after its trailing blank lines; an empty `text` keeps room above the signature.
pub fn sign(text: &str, signature: &str) -> String {
    format!(
        "{}\n\n-- \n{}",
        text.trim_end_matches(['\n', '\r']),
        signature.trim_end_matches(['\n', '\r'])
    )
}

/// Whether `text` ends with what [`sign`] puts there for `signature`.
pub fn signed_by(text: &str, signature: &str) -> bool {
    text.trim_end_matches(['\n', '\r'])
        .ends_with(&sign("", signature))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_trims_both_and_puts_the_delimiter_between() {
        assert_eq!(sign("Hi\n\n", "P\n"), "Hi\n\n-- \nP");
        assert_eq!(sign("", "P"), "\n\n-- \nP");
    }

    #[test]
    fn signed_by_knows_its_own_signature() {
        assert!(signed_by(&sign("Hi", "P"), "P"));
        assert!(!signed_by("Hi\n\n-- \nQ", "P"));
    }
}
