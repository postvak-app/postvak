//! IMAP's modified UTF-7 for folder names (RFC 3501 §5.1.3): how a name travels on the wire. `ImapOps` encodes every
//! name it sends and decodes every name the server lists, so everything above `MailOps` uses UTF-8.

/// The modified BASE64 alphabet: RFC 2045's with `,` in place of `/`.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// `name` as the server expects it: printable ASCII as itself, `&` as `&-`, and every run of other characters as
/// `&`, the modified BASE64 of its UTF-16, `-`.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut run: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut run, &mut out);
            out.push(c);
            if c == '&' {
                out.push('-');
            }
        } else {
            let mut units = [0u16; 2];
            run.extend_from_slice(c.encode_utf16(&mut units));
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Writes `run` as one `&…-` section and empties it.
fn flush(run: &mut Vec<u16>, out: &mut String) {
    if run.is_empty() {
        return;
    }
    out.push('&');
    let (mut bits, mut count) = (0u32, 0u32);
    for unit in run.drain(..) {
        bits = (bits << 16) | u32::from(unit);
        count += 16;
        while count >= 6 {
            count -= 6;
            out.push(char::from(ALPHABET[((bits >> count) & 0x3f) as usize]));
        }
        bits &= (1 << count) - 1;
    }
    if count > 0 {
        out.push(char::from(
            ALPHABET[((bits << (6 - count)) & 0x3f) as usize],
        ));
    }
    out.push('-');
}

/// The UTF-8 name of a name the server sent; `None` unless `wire` is modified UTF-7 exactly as `encode` writes it, so
/// `encode(decode(wire)) == wire` and the server gets back the name it listed. A server that sends raw UTF-8 or a
/// sloppy encoding gets `None`, and its name is used as it came.
pub fn decode(wire: &str) -> Option<String> {
    let mut out = String::with_capacity(wire.len());
    let mut rest = wire;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('-')?;
        let section = &after[..end];
        if section.is_empty() {
            out.push('&');
        } else {
            out.push_str(&decode_section(section)?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    (encode(&out) == wire).then_some(out)
}

/// The text of one `&…-` section's BASE64, or `None` when it is not whole UTF-16 with zero bits left over.
fn decode_section(section: &str) -> Option<String> {
    let mut units = Vec::with_capacity(section.len() * 6 / 16);
    let (mut bits, mut count) = (0u32, 0u32);
    for byte in section.bytes() {
        let value = ALPHABET.iter().position(|&a| a == byte)?;
        bits = (bits << 6) | value as u32;
        count += 6;
        if count >= 16 {
            count -= 16;
            units.push(((bits >> count) & 0xffff) as u16);
            bits &= (1 << count) - 1;
        }
    }
    if bits != 0 || count >= 6 {
        return None;
    }
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_3501_example_round_trips() {
        // RFC 3501 §5.1.3: "~peter/mail/台北/日本語".
        let name = "~peter/mail/\u{53f0}\u{5317}/\u{65e5}\u{672c}\u{8a9e}";
        let wire = "~peter/mail/&U,BTFw-/&ZeVnLIqe-";
        assert_eq!(encode(name), wire);
        assert_eq!(decode(wire).as_deref(), Some(name));
    }

    #[test]
    fn an_ampersand_is_written_as_ampersand_dash() {
        assert_eq!(encode("R&D"), "R&-D");
        assert_eq!(decode("R&-D").as_deref(), Some("R&D"));
        assert_eq!(encode("&&"), "&-&-");
    }

    #[test]
    fn printable_ascii_stays_as_it_is() {
        let ascii: String = (' '..='~').filter(|&c| c != '&').collect();
        assert_eq!(encode(&ascii), ascii);
        assert_eq!(decode(&ascii).as_deref(), Some(ascii.as_str()));
    }

    #[test]
    fn accented_letters_and_mixed_scripts_round_trip() {
        for (name, wire) in [
            ("Archief/\u{c9}\u{e9}n", "Archief/&AMkA6Q-n"),
            ("Caf\u{e9}", "Caf&AOk-"),
            ("\u{e9}", "&AOk-"),
            ("Re\u{e7}us & \u{91cd}\u{8981}", "Re&AOc-us &- &kc2JgQ-"),
        ] {
            assert_eq!(encode(name), wire, "{name}");
            assert_eq!(decode(wire).as_deref(), Some(name), "{wire}");
        }
    }

    #[test]
    fn characters_outside_the_bmp_use_surrogate_pairs() {
        let name = "Mail \u{1f4ec}";
        let wire = encode(name);
        assert_eq!(wire, "Mail &2D3c7A-");
        assert_eq!(decode(&wire).as_deref(), Some(name));
    }

    #[test]
    fn control_characters_are_encoded_not_sent_raw() {
        assert_eq!(encode("a\u{7f}"), "a&AH8-");
        assert_eq!(decode("a&AH8-").as_deref(), Some("a\u{7f}"));
    }

    #[test]
    fn what_encode_would_not_write_does_not_decode() {
        for wire in [
            // Raw UTF-8, as some servers send.
            "Caf\u{e9}",
            // No closing dash.
            "Caf&AOk",
            // A character outside the alphabet.
            "&AO/-",
            // Leftover bits that are not zero.
            "&AOl-",
            // A printable character encoded.
            "&AGE-",
            // Two sections side by side, which encode writes as one.
            "&AOk-&AOk-",
            // A lone surrogate.
            "&2D0-",
            // A whole sextet left over.
            "&AOkA-",
            // A bare & is a literal & on no server's side of RFC 3501.
            "R&D",
        ] {
            assert_eq!(decode(wire), None, "{wire}");
        }
    }

    #[test]
    fn every_name_round_trips() {
        for name in [
            "",
            "INBOX",
            "a-b",
            "&",
            "-&-",
            "\u{0}",
            "\u{ffff}",
            "\u{10ffff}x",
        ] {
            assert_eq!(decode(&encode(name)).as_deref(), Some(name), "{name:?}");
        }
    }
}
