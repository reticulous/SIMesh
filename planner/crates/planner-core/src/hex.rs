//! Lowercase hexadecimal, in ONE place.
//!
//! This is six lines of code and it lives here rather than in each crate that
//! needs it, for the same reason the record codec now lives in one crate: this
//! tree has already paid for a duplicated codec once. `planner-cli` writes node
//! identifiers into a log, `planner-pack` decodes the identifiers it holds in
//! order to match them, and a device id printed in two different cases in two
//! places is a device id that will not match itself.
//!
//! Not a dependency because `hex` the crate would be a build-graph entry for
//! two functions, and this project builds offline.

/// Lowercase hex, two characters per byte.
pub fn encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).expect("a nibble is 0..=15"));
        s.push(char::from_digit((b & 0x0F) as u32, 16).expect("a nibble is 0..=15"));
    }
    s
}

/// Decode a hex string of EXACTLY `N` bytes.
///
/// Exact rather than tolerant on purpose: the two callers both decode
/// fixed-width identifiers (an 8-byte device id, a 32-byte public key), and a
/// decoder that accepted a short string would silently turn a truncated field
/// into a valid-looking identifier of the wrong length. `None` covers wrong
/// length, non-hex characters and surrounding junk alike; the caller decides
/// what to say about it, because "this id is not a key" means something
/// different in each place.
pub fn decode_exact<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (b[i * 2] as char).to_digit(16)?;
        let lo = (b[i * 2 + 1] as char).to_digit(16)?;
        *o = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_and_decoding_are_inverses_over_the_whole_byte_range() {
        let all: Vec<u8> = (0..=255u8).collect();
        let hex = encode(&all);
        assert_eq!(hex.len(), 512);
        assert_eq!(decode_exact::<256>(&hex).map(|a| a.to_vec()), Some(all));
    }

    #[test]
    fn hex_is_always_lowercase_so_two_writers_produce_one_spelling() {
        assert_eq!(encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
    }

    #[test]
    fn a_string_of_the_wrong_length_is_refused_rather_than_padded() {
        // A truncated identifier that decoded to a short array would look
        // exactly like a legitimate shorter one.
        assert_eq!(decode_exact::<4>("deadbe"), None);
        assert_eq!(decode_exact::<4>("deadbeef00"), None);
        assert_eq!(decode_exact::<4>(" deadbeef"), None);
    }

    #[test]
    fn a_non_hex_character_is_refused_rather_than_read_as_zero() {
        assert_eq!(decode_exact::<2>("zz00"), None);
        assert_eq!(decode_exact::<2>("00zz"), None);
    }

    #[test]
    fn uppercase_hex_still_decodes_because_other_tools_emit_it() {
        assert_eq!(decode_exact::<2>("DEAD"), Some([0xDE, 0xAD]));
    }
}
