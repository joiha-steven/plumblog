//! Base64url without padding (RFC 4648 section 5), the only encoding auth secrets travel in.
//! Decoding is strict so one value has exactly one spelling: no padding, no whitespace, no
//! non-zero trailing bits. A lenient decoder would let two different strings match one secret.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        // chunk.len() + 1 output chars carry all input bits; the rest would be padding.
        for i in 0..=chunk.len() {
            let idx = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(ALPHABET[idx as usize]));
        }
    }
    out
}

fn value(c: u8) -> Option<u32> {
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'-' => 62,
        b'_' => 63,
        _ => return None,
    };
    Some(u32::from(v))
}

/// `None` for any character outside the alphabet, padding, an impossible length
/// (`len % 4 == 1`) or non-zero unused trailing bits.
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let input = text.as_bytes();
    if input.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 2);
    for chunk in input.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        // 2 chars -> 1 byte, 3 -> 2, 4 -> 3; the bits below them must be zero.
        let keep = chunk.len() - 1;
        if bytes[1 + keep..].iter().any(|b| *b != 0) {
            return None;
        }
        out.extend_from_slice(&bytes[1..=keep]);
    }
    Some(out)
}

/// Lowercase hex, the form hashes are stored in.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, hex};

    #[test]
    fn rfc4648_vectors_round_trip() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (raw, text) in cases {
            assert_eq!(encode(raw), text);
            assert_eq!(decode(text).as_deref(), Some(raw));
        }
        assert_eq!(encode(&[0xfb, 0xff, 0xbf]), "-_-_");
        assert_eq!(decode("-_-_"), Some(vec![0xfb, 0xff, 0xbf]));
    }

    #[test]
    fn every_byte_length_round_trips() {
        let data: Vec<u8> = (0..=255u8).collect();
        for n in 0..data.len() {
            assert_eq!(decode(&encode(&data[..n])).as_deref(), Some(&data[..n]));
        }
    }

    #[test]
    fn rejects_non_canonical_input() {
        for bad in [
            "Zg==", "Zg=", "Z", "Zh", "Zm9=", "Zm+v", "Zm/v", "Zm 9v", " Zm9v", "Zm9v\n", "Zm9w\0",
            "\u{e9}a",
        ] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
        // "Zh" and "Zm9" differ from canonical "Zg" / "Zm8" only in unused bits.
        assert_eq!(decode("Zm9"), None);
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0x7f, 0xff]), "00ab7fff");
        assert_eq!(hex(&[]), "");
    }
}
