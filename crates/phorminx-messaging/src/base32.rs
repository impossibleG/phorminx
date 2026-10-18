//! Lowercase RFC 4648 base32 without padding (the alphabet onion addresses use).

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes case-insensitively. Rejects foreign characters and non-zero
/// trailing bits, so every byte string has exactly one accepted encoding.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = match c.to_ascii_lowercase() {
            c @ b'a'..=b'z' => c - b'a',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    if bits >= 5 || buffer & ((1 << bits) - 1) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "my"),
            ("fo", "mzxq"),
            ("foo", "mzxw6"),
            ("foob", "mzxw6yq"),
            ("fooba", "mzxw6ytb"),
            ("foobar", "mzxw6ytboi"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded);
            assert_eq!(decode(encoded).unwrap(), plain.as_bytes());
            assert_eq!(decode(&encoded.to_uppercase()).unwrap(), plain.as_bytes());
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decode("mzxw6!").is_none());
        assert!(decode("m1").is_none());
        // "mz" has non-zero trailing bits.
        assert!(decode("mz").is_none());
        // A lone trailing symbol cannot encode a byte.
        assert!(decode("mzxw6ytbo").is_none());
    }

    #[test]
    fn roundtrips_all_lengths() {
        let data: Vec<u8> = (0..=255).collect();
        for len in 0..64 {
            assert_eq!(decode(&encode(&data[..len])).unwrap(), &data[..len]);
        }
    }
}
