//! Legacy colour attributes (`bgcolor`, `<font color>`), which old mail still uses. Blitz reads only `#rgb` and
//! `#rrggbb`, and reads each digit of `#rgb` as 0 to 15 without doubling it, so `#ddd` paints near black. The parse
//! rewrites such an attribute to `#rrggbb`, by the HTML rules, before Blitz sees it.
use color::{AlphaColor, Srgb};

/// The attributes holding a legacy colour, by element: `bgcolor` on any, `color` on `<font>`.
pub fn is_colour_attribute(element: &str, attribute: &str) -> bool {
    attribute == "bgcolor" || (attribute == "color" && element == "font")
}

/// `value` as `#rrggbb`, by the HTML "rules for parsing a legacy colour value"; None where the attribute maps to no
/// colour (empty, or `transparent`).
///
/// <https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#rules-for-parsing-a-legacy-colour-value>
pub fn parse(value: &str) -> Option<String> {
    let value = value.trim_matches(|c: char| c.is_ascii_whitespace());
    if value.is_empty() || value.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some([r, g, b]) = named(value) {
        return Some(format!("#{r:02x}{g:02x}{b:02x}"));
    }
    // `#rgb`: each digit doubled.
    let mut chars = value.chars();
    if let (Some('#'), Some(r), Some(g), Some(b), None) = (
        chars.next(),
        chars.next(),
        chars.next(),
        chars.next(),
        chars.next(),
    ) && [r, g, b].iter().all(char::is_ascii_hexdigit)
    {
        return Some(format!("#{r}{r}{g}{g}{b}{b}").to_ascii_lowercase());
    }
    // Otherwise leniently: every character that is not a hex digit counts as 0, and the digits split in three.
    let digits: Vec<char> = value
        .chars()
        // A code point past U+FFFF counts as two zeros.
        .flat_map(|c| {
            if u32::from(c) > 0xFFFF {
                vec!['0', '0']
            } else {
                vec![c]
            }
        })
        .take(128)
        .collect();
    let digits = digits.strip_prefix(&['#']).unwrap_or(&digits);
    let mut digits: Vec<char> = digits
        .iter()
        .map(|&c| if c.is_ascii_hexdigit() { c } else { '0' })
        .collect();
    while digits.is_empty() || !digits.len().is_multiple_of(3) {
        digits.push('0');
    }
    let mut length = digits.len() / 3;
    let mut parts: Vec<&[char]> = digits.chunks(length).collect();
    if length > 8 {
        parts = parts.iter().map(|p| &p[length - 8..]).collect();
        length = 8;
    }
    while length > 2 && parts.iter().all(|p| p[0] == '0') {
        parts = parts.iter().map(|p| &p[1..]).collect();
        length -= 1;
    }
    let [r, g, b] = [parts[0], parts[1], parts[2]].map(|p| p.iter().take(2).collect::<String>());
    // A component of one digit is that digit's value, as a whole byte.
    let pad = |s: String| if s.len() == 1 { format!("0{s}") } else { s };
    Some(format!("#{}{}{}", pad(r), pad(g), pad(b)).to_ascii_lowercase())
}

/// A CSS colour name, matched without regard to case.
fn named(value: &str) -> Option<[u8; 3]> {
    if !value.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let colour: AlphaColor<Srgb> = color::parse_color(value).ok()?.to_alpha_color();
    let [r, g, b, a] = colour.to_rgba8().to_u8_array();
    (a == 255).then_some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_hex_and_three_digit_values_become_six_digit_hex() {
        assert_eq!(parse("red").as_deref(), Some("#ff0000"));
        assert_eq!(parse(" Red ").as_deref(), Some("#ff0000"));
        assert_eq!(parse("rebeccapurple").as_deref(), Some("#663399"));
        assert_eq!(parse("#ddd").as_deref(), Some("#dddddd"));
        assert_eq!(parse("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(parse("#123456").as_deref(), Some("#123456"));
        assert_eq!(parse("ff8800").as_deref(), Some("#ff8800"));
    }

    #[test]
    fn nothing_and_transparent_map_to_no_colour() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("  "), None);
        assert_eq!(parse("Transparent"), None);
    }

    #[test]
    fn junk_follows_the_spec_not_an_error() {
        // The spec's own example: "chucknorris" is a dark red.
        assert_eq!(parse("chucknorris").as_deref(), Some("#c00000"));
        // Non-hex characters count as 0, short values pad.
        assert_eq!(parse("#zz").as_deref(), Some("#000000"));
        assert_eq!(parse("f").as_deref(), Some("#0f0000"));
        // Long components keep their first two digits unless they start with zeros.
        assert_eq!(parse("#ff00ff00ff00").as_deref(), Some("#ffffff"));
        assert_eq!(parse("#00ff00ff00ff").as_deref(), Some("#ffffff"));
    }

    #[test]
    fn only_the_attributes_that_hold_a_colour_are_rewritten() {
        assert!(is_colour_attribute("td", "bgcolor"));
        assert!(is_colour_attribute("font", "color"));
        assert!(!is_colour_attribute("td", "color"));
        assert!(!is_colour_attribute("td", "width"));
    }
}
