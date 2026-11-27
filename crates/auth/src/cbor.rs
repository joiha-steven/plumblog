//! Minimal CBOR reader (RFC 8949) for `WebAuthn` attestation objects and COSE keys.
//!
//! Input comes straight from the network, so: definite lengths only (no indefinite items, tags,
//! floats or simple values beyond bool/null), nesting at most [`MAX_DEPTH`], at most
//! [`MAX_ITEMS`] items in total, every declared length checked against the bytes actually left
//! before anything is allocated, duplicate map keys refused. Every problem is an `Err`; nothing
//! here panics. Strings borrow from the input, so decoding never copies payloads.

use plumblog_model::{Error, ErrorCode, Result};

/// Deepest container nesting accepted (a top-level map is depth 1).
pub const MAX_DEPTH: usize = 8;
/// Most items (containers, keys and values all count) in one decode. A real attestation object
/// has about 30; the cap bounds allocation and the quadratic duplicate-key check.
pub const MAX_ITEMS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Unsigned(u64),
    /// Major type 1: the value is `-1 - n`.
    Negative(u64),
    Bytes(&'a [u8]),
    Text(&'a str),
    Array(Vec<Value<'a>>),
    Map(Vec<(Value<'a>, Value<'a>)>),
    Bool(bool),
    Null,
}

impl<'a> Value<'a> {
    /// The integer value, for major types 0 and 1 (every CBOR integer fits an `i128`).
    #[must_use]
    pub fn as_int(&self) -> Option<i128> {
        match *self {
            Self::Unsigned(n) => Some(i128::from(n)),
            Self::Negative(n) => Some(-1 - i128::from(n)),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match *self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&'a str> {
        match *self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_map(&self) -> Option<&[(Value<'a>, Value<'a>)]> {
        match self {
            Self::Map(m) => Some(m),
            _ => None,
        }
    }

    /// Value under the integer key `key` of a map (keys are unique, see [`decode`]).
    #[must_use]
    pub fn get_int(&self, key: i128) -> Option<&Value<'a>> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k.as_int() == Some(key))
            .map(|(_, v)| v)
    }

    /// Value under the text key `key` of a map.
    #[must_use]
    pub fn get_text(&self, key: &str) -> Option<&Value<'a>> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k.as_text() == Some(key))
            .map(|(_, v)| v)
    }
}

fn err(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("cbor: {}", detail.into()))
}

/// Decodes exactly one item that must span all of `input`.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] for malformed, truncated, unsupported or trailing data.
pub fn decode(input: &[u8]) -> Result<Value<'_>> {
    let (value, used) = decode_prefix(input)?;
    if used != input.len() {
        return Err(err(format!(
            "{} trailing bytes after item",
            input.len() - used
        )));
    }
    Ok(value)
}

/// Decodes one item from the start of `input` and returns it with the bytes it used, for
/// formats that put CBOR in the middle of a binary structure (`WebAuthn` authenticator data).
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] for malformed, truncated or unsupported data.
pub fn decode_prefix(input: &[u8]) -> Result<(Value<'_>, usize)> {
    let mut reader = Reader {
        input,
        pos: 0,
        items: 0,
    };
    let value = reader.item(0)?;
    Ok((value, reader.pos))
}

struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
    items: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.input.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(err(format!(
                "need {n} bytes at offset {}, {} left",
                self.pos,
                self.remaining()
            )));
        }
        let out = &self.input[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Reads an initial byte and its argument: `(major type, additional info, argument)`.
    fn head(&mut self) -> Result<(u8, u8, u64)> {
        let first = self.take(1)?[0];
        let (major, info) = (first >> 5, first & 0x1f);
        let arg = match info {
            0..=23 => u64::from(info),
            24..=27 => {
                let n = 1usize << (info - 24);
                self.take(n)?
                    .iter()
                    .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
            }
            31 => return Err(err(format!("indefinite length at offset {}", self.pos - 1))),
            _ => return Err(err(format!("reserved additional info {info}"))),
        };
        Ok((major, info, arg))
    }

    /// A declared byte length, refused unless that many bytes are actually left.
    fn length(&self, arg: u64) -> Result<usize> {
        match usize::try_from(arg) {
            Ok(n) if n <= self.remaining() => Ok(n),
            _ => Err(err(format!(
                "declared length {arg} exceeds {} bytes left",
                self.remaining()
            ))),
        }
    }

    /// Reserves `count` more items, so a container cannot declare more than can follow.
    fn reserve(&mut self, count: u64) -> Result<usize> {
        let n = usize::try_from(count).map_err(|_| err("item count overflows"))?;
        // Each item takes at least one byte, so `n` items cannot fit in fewer bytes.
        if n > self.remaining() {
            return Err(err(format!(
                "{n} items declared, {} bytes left",
                self.remaining()
            )));
        }
        match self.items.checked_add(n) {
            Some(total) if total <= MAX_ITEMS => {
                self.items = total;
                Ok(n)
            }
            _ => Err(err(format!("more than {MAX_ITEMS} items"))),
        }
    }

    fn item(&mut self, depth: usize) -> Result<Value<'a>> {
        if depth == 0 {
            self.reserve(1)?;
        }
        let (major, info, arg) = self.head()?;
        match major {
            0 => Ok(Value::Unsigned(arg)),
            1 => Ok(Value::Negative(arg)),
            2 => {
                let n = self.length(arg)?;
                Ok(Value::Bytes(self.take(n)?))
            }
            3 => {
                let n = self.length(arg)?;
                let raw = self.take(n)?;
                std::str::from_utf8(raw)
                    .map(Value::Text)
                    .map_err(|_| err("text string is not UTF-8"))
            }
            4 => {
                Self::enter(depth)?;
                let n = self.reserve(arg)?;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(self.item(depth + 1)?);
                }
                Ok(Value::Array(out))
            }
            5 => {
                Self::enter(depth)?;
                let pairs = arg
                    .checked_mul(2)
                    .ok_or_else(|| err("map size overflows"))?;
                let n = self.reserve(pairs)? / 2;
                let mut out: Vec<(Value<'a>, Value<'a>)> = Vec::with_capacity(n);
                for _ in 0..n {
                    let key = self.item(depth + 1)?;
                    if out.iter().any(|(k, _)| *k == key) {
                        return Err(err("duplicate map key"));
                    }
                    let value = self.item(depth + 1)?;
                    out.push((key, value));
                }
                Ok(Value::Map(out))
            }
            6 => Err(err("tags are not supported")),
            _ => match (info, arg) {
                (20, _) => Ok(Value::Bool(false)),
                (21, _) => Ok(Value::Bool(true)),
                (22, _) => Ok(Value::Null),
                _ => Err(err(format!("unsupported simple/float value (info {info})"))),
            },
        }
    }

    fn enter(depth: usize) -> Result<()> {
        if depth + 1 > MAX_DEPTH {
            return Err(err(format!("nesting deeper than {MAX_DEPTH}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DEPTH, MAX_ITEMS, Value, decode, decode_prefix};
    use crate::testutil::Prng;
    use plumblog_model::ErrorCode;

    /// Hand-encoded samples covering every supported type and argument width.
    fn samples() -> Vec<Vec<u8>> {
        let mut long_text = vec![0x78, 30];
        long_text.extend_from_slice(&[b'a'; 30]);
        vec![
            vec![0x00],
            vec![0x17],
            vec![0x18, 0x18],
            vec![0x19, 0x01, 0x00],
            vec![0x1a, 0x00, 0x01, 0x00, 0x00],
            vec![0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            vec![0x20],
            vec![0x38, 0x63],
            vec![0x39, 0x01, 0x00],
            vec![0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            vec![0x43, 1, 2, 3],
            vec![0x63, b'a', b'b', b'c'],
            long_text,
            vec![0xf4],
            vec![0xf5],
            vec![0xf6],
            // [1, [2, 3], {"a": h'00'}]
            vec![0x83, 0x01, 0x82, 0x02, 0x03, 0xa1, 0x61, b'a', 0x41, 0x00],
            // {1: 2, 3: -7, -1: 1, -2: h'..', -3: h'..'} a COSE ES256 key shape
            crate::webauthn::tests::cose_es256(&[7; 32], &[9; 32]),
            // {"fmt": "none", "attStmt": {}, "authData": h'0102'}
            vec![
                0xa3, 0x63, b'f', b'm', b't', 0x64, b'n', b'o', b'n', b'e', 0x67, b'a', b't', b't',
                b'S', b't', b'm', b't', 0xa0, 0x68, b'a', b'u', b't', b'h', b'D', b'a', b't', b'a',
                0x42, 1, 2,
            ],
        ]
    }

    #[test]
    fn decodes_each_type() {
        assert_eq!(decode(&[0x00]).unwrap(), Value::Unsigned(0));
        assert_eq!(decode(&[0x18, 0x18]).unwrap().as_int(), Some(24));
        assert_eq!(decode(&[0x20]).unwrap().as_int(), Some(-1));
        assert_eq!(decode(&[0x38, 0x63]).unwrap().as_int(), Some(-100));
        assert_eq!(decode(&[0x39, 0x01, 0x00]).unwrap().as_int(), Some(-257));
        assert_eq!(
            decode(&[0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])
                .unwrap()
                .as_int(),
            Some(-1 - i128::from(u64::MAX))
        );
        assert_eq!(decode(&[0x43, 1, 2, 3]).unwrap(), Value::Bytes(&[1, 2, 3]));
        assert_eq!(
            decode(&[0x63, b'a', b'b', b'c']).unwrap(),
            Value::Text("abc")
        );
        assert_eq!(decode(&[0xf4]).unwrap(), Value::Bool(false));
        assert_eq!(decode(&[0xf5]).unwrap(), Value::Bool(true));
        assert_eq!(decode(&[0xf6]).unwrap(), Value::Null);
        let all = samples();
        let v = decode(&all[16]).unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Unsigned(1),
                Value::Array(vec![Value::Unsigned(2), Value::Unsigned(3)]),
                Value::Map(vec![(Value::Text("a"), Value::Bytes(&[0]))]),
            ])
        );
        let att = decode(all.last().unwrap()).unwrap();
        assert_eq!(att.get_text("fmt"), Some(&Value::Text("none")));
        assert_eq!(att.get_text("authData"), Some(&Value::Bytes(&[1, 2])));
        assert_eq!(att.get_text("missing"), None);
    }

    #[test]
    fn prefix_reports_bytes_used_and_full_decode_refuses_trailing() {
        assert_eq!(
            decode_prefix(&[0x41, 9, 0xff, 0xff]).unwrap(),
            (Value::Bytes(&[9]), 2)
        );
        assert!(decode(&[0x41, 9, 0x00]).is_err());
        assert!(decode(&[]).is_err());
    }

    #[test]
    fn every_truncation_of_every_sample_is_an_error() {
        let mut cases = 0;
        for sample in samples() {
            assert!(decode(&sample).is_ok(), "{sample:02x?}");
            for cut in 0..sample.len() {
                let e = decode(&sample[..cut]).unwrap_err();
                assert_eq!(e.code, ErrorCode::InvalidInput);
                cases += 1;
            }
        }
        assert!(cases > 150, "{cases}");
    }

    #[test]
    fn rejects_unsupported_items() {
        let bad: &[&[u8]] = &[
            &[0x5f, 0x41, 0x00, 0xff],                   // indefinite byte string
            &[0x7f, 0xff],                               // indefinite text
            &[0x9f, 0xff],                               // indefinite array
            &[0xbf, 0xff],                               // indefinite map
            &[0xff],                                     // break outside indefinite item
            &[0x1c],                                     // reserved additional info 28
            &[0x1d],                                     // 29
            &[0x1e],                                     // 30
            &[0xc0, 0x00],                               // tag
            &[0xd8, 0x18, 0x41, 0x00],                   // tag 24 (embedded CBOR)
            &[0xf7],                                     // undefined
            &[0xf0],                                     // simple value 16
            &[0xf8, 0x20],                               // simple value 32
            &[0xf9, 0x3c, 0x00],                         // half float
            &[0xfa, 0, 0, 0, 0],                         // single float
            &[0xfb, 0, 0, 0, 0, 0, 0, 0, 0],             // double float
            &[0x62, 0xc3, 0x28],                         // invalid UTF-8
            &[0x61, 0x80],                               // lone continuation byte
            &[0xa2, 0x01, 0x02, 0x01, 0x03],             // duplicate int key
            &[0xa2, 0x61, b'a', 0x00, 0x61, b'a', 0x01], // duplicate text key
        ];
        for input in bad {
            let e = decode(input).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{input:02x?}");
        }
    }

    #[test]
    fn huge_declared_lengths_fail_without_allocating() {
        let huge: &[&[u8]] = &[
            &[0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            &[0x7b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            &[0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            &[0xbb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            &[0xbb, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // 2 * n overflows
            &[0x5a, 0xff, 0xff, 0xff, 0xff, 0x00],
            &[0x9a, 0x00, 0x01, 0x00, 0x00, 0x00],
            &[0x82, 0x00], // array of 2, one present
        ];
        for input in huge {
            assert!(decode(input).is_err(), "{input:02x?}");
        }
    }

    #[test]
    fn nesting_is_limited() {
        let ok: Vec<u8> = std::iter::repeat_n(0x81, MAX_DEPTH).chain([0x00]).collect();
        assert!(decode(&ok).is_ok());
        let deep: Vec<u8> = std::iter::repeat_n(0x81, MAX_DEPTH + 1)
            .chain([0x00])
            .collect();
        assert!(decode(&deep).is_err());
        // A bomb a naive recursive reader would overflow the stack on.
        let bomb = vec![0x81; 100_000];
        assert!(decode(&bomb).is_err());
        let map_bomb: Vec<u8> = std::iter::repeat_n([0xa1, 0x00], 50_000)
            .flatten()
            .collect();
        assert!(decode(&map_bomb).is_err());
    }

    #[test]
    fn item_count_is_limited() {
        let mut ok = vec![0x99];
        ok.extend_from_slice(&u16::try_from(MAX_ITEMS - 1).unwrap().to_be_bytes());
        ok.extend(std::iter::repeat_n(0x00, MAX_ITEMS - 1));
        assert!(decode(&ok).is_ok());
        let mut over = vec![0x99];
        over.extend_from_slice(&u16::try_from(MAX_ITEMS).unwrap().to_be_bytes());
        over.extend(std::iter::repeat_n(0x00, MAX_ITEMS));
        assert!(decode(&over).is_err());
    }

    /// Random inputs, plus random single-byte mutations of valid samples: every result is an
    /// `Ok` or an `Err`, never a panic, and any `Ok` re-checks as a full decode.
    #[test]
    fn random_bytes_never_panic() {
        let mut rng = Prng(0x0123_4567_89ab_cdef);
        let samples = samples();
        let mut oks = 0;
        for i in 0..20_000 {
            let input = if i % 2 == 0 {
                let len = rng.below(64);
                rng.bytes(len)
            } else {
                let mut s = samples[rng.below(samples.len())].clone();
                let at = rng.below(s.len());
                s[at] = rng.bytes(1)[0];
                s
            };
            if let Ok((_, used)) = decode_prefix(&input) {
                assert!(used <= input.len());
                assert!(decode(&input[..used]).is_ok());
                oks += 1;
            }
        }
        assert!(oks > 0);
    }
}
