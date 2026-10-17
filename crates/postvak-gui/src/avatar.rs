//! The colour behind a sender's initials, picked from their address so it never changes.
use super::theme::TINTS;

/// Which avatar fill `address` gets: FNV-1a of the lowercased address, so it holds across runs and releases.
pub(crate) fn tint(address: &str) -> usize {
    let hash = address
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    usize::try_from(hash % TINTS as u64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn the_tint_is_stable_ignores_case_and_spreads() {
        assert_eq!(tint("ada@example.com"), tint("Ada@Example.com"));
        let tints: HashSet<usize> = ["ada", "alan", "grace", "linus", "mum", "shop", "news"]
            .iter()
            .map(|name| tint(&format!("{name}@example.com")))
            .collect();
        assert!(tints.len() > 2, "{tints:?}");
        assert!(tints.iter().all(|&t| t < TINTS));
        // Pinned: a change would recolour everyone's avatars.
        assert_eq!(tint("linus@example.com"), 1);
    }
}
