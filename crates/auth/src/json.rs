//! A tiny strict JSON reader, UTF-8 only, size-capped by the caller, no duplicate keys.
//! Anything else is refused rather than guessed at. Two shapes:
//! - [`parse_object`]: one flat object of strings and booleans, for `clientDataJSON` and the
//!   `/api/auth` bodies.
//! - [`parse_document`]: one object of any JSON values except floats (integers only, in the
//!   i64 range), nesting and container sizes capped, for `/api/v1` bodies.

use plumblog_model::{Error, ErrorCode, Result};
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Str(String),
    Bool(bool),
}

/// A value of [`parse_document`]. Objects keep key order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

/// Deepest nesting [`parse_document`] accepts, the top object being 1. Bounds recursion, so
/// hostile input cannot exhaust the stack.
pub const MAX_DEPTH: usize = 16;
/// Most items in one array or object.
pub const MAX_ITEMS: usize = 1000;
/// Longest key, in bytes: keys are field names, never data.
pub const MAX_KEY: usize = 64;
/// Most values in one [`parse_document`] document: array items plus object members, at every
/// depth. Bounds the whole parse, not one container: 160 KiB of 1000-key objects (22,000
/// members) took ~1.2 ms natively and ~3 ms on a CI runner, too close to the Worker's 10 ms
/// once in Wasm. Real bodies are far smaller: a post is ~15 members plus its tags, a theme
/// option list at most 20 items, a color 2, a setting a few dozen (nav), and tokens,
/// redirects and media metadata a handful (media metadata is in the query). 4096 still lets
/// one container reach [`MAX_ITEMS`] four times over, so no cap is reachable before it.
pub const MAX_VALUES: usize = 4096;

/// Whether a string byte ends a plain run: quote, backslash or control character.
#[inline]
fn special(b: u8) -> bool {
    b == b'"' || b == b'\\' || b < 0x20
}

/// Length of the plain run at the start of `bytes`, if it ends before the input does. Reads
/// eight bytes per step: a byte at a time was most of the parse time (`tests/perf.rs`).
fn plain_run(bytes: &[u8]) -> Option<usize> {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    // Non-zero iff some byte of `w` is below `n` (n <= 128); exact as a yes/no.
    let below = |w: u64, n: u8| w.wrapping_sub(ONES * u64::from(n)) & !w & HIGH;
    let mut i = 0;
    while let Some(chunk) = bytes.get(i..i + 8) {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let w = u64::from_le_bytes(word);
        if below(w ^ (ONES * u64::from(b'"')), 1)
            | below(w ^ (ONES * u64::from(b'\\')), 1)
            | below(w, 0x20)
            != 0
        {
            break;
        }
        i += 8;
    }
    bytes[i..].iter().position(|&b| special(b)).map(|p| i + p)
}

// Hot paths call this on failure only; kept out of line so they stay small and inlined.
#[cold]
fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("json: {}", detail.into()))
}

/// Objects up to this many keys are checked for duplicates by a plain scan: cheaper than
/// hashing for the small objects nearly every request sends.
const SCAN_KEYS: usize = 8;

/// Duplicate-key check for one object. Scanning every key is quadratic: 160 KiB of 1000-key
/// objects took ~20 ms natively (`tests/perf.rs`). Up to [`SCAN_KEYS`] keys, each new key is
/// compared with the others; past it, each key is hashed once with `std`'s randomly keyed
/// hasher (so crafted keys cannot be made to collide) and the hashes are sorted when the
/// object ends, which beat a growing hash table here.
#[derive(Default)]
struct Keys {
    hashes: Vec<(u64, usize)>,
    hasher: RandomState,
}

impl Keys {
    /// Whether `key` repeats one of a small object's `fields`; past [`SCAN_KEYS`], remembers
    /// its hash for [`Keys::repeated`] and answers `false`.
    fn duplicate<V>(&mut self, fields: &[(String, V)], key: &str) -> bool {
        if fields.len() < SCAN_KEYS {
            return fields.iter().any(|(k, _)| k == key);
        }
        if self.hashes.is_empty() {
            for (i, (k, _)) in fields.iter().enumerate() {
                self.hashes.push((self.hasher.hash_one(k.as_str()), i));
            }
        }
        self.hashes.push((self.hasher.hash_one(key), fields.len()));
        false
    }

    /// A key that appears twice among the hashed ones, once the object is complete.
    fn repeated<'f, V>(&mut self, fields: &'f [(String, V)]) -> Option<&'f str> {
        self.hashes.sort_unstable();
        let mut run = 0;
        for i in 1..=self.hashes.len() {
            if i < self.hashes.len() && self.hashes[i].0 == self.hashes[run].0 {
                continue;
            }
            // Keys with one 64-bit hash: equal, or (as good as never) a collision.
            let same = &self.hashes[run..i];
            for (n, (_, a)) in same.iter().enumerate() {
                if same[n + 1..]
                    .iter()
                    .any(|(_, b)| fields[*a].0 == fields[*b].0)
                {
                    return Some(&fields[*a].0);
                }
            }
            run = i;
        }
        None
    }
}

struct Parser<'a> {
    /// Already checked to be UTF-8; plain runs of strings are sliced from it, not re-checked.
    text: &'a str,
    input: &'a [u8],
    pos: usize,
    /// Values read so far, for [`MAX_VALUES`].
    values: usize,
}

impl Parser<'_> {
    /// Counts one more value; refuses the document the moment it passes [`MAX_VALUES`].
    #[inline]
    fn count_value(&mut self) -> Result<()> {
        self.values += 1;
        if self.values > MAX_VALUES {
            return Err(Error::new(
                ErrorCode::TooComplex,
                format!("json: more than {MAX_VALUES} values"),
            ));
        }
        Ok(())
    }

    #[inline]
    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.input.get(self.pos) {
            self.pos += 1;
        }
    }

    #[inline]
    fn next(&mut self) -> Result<u8> {
        let b = *self
            .input
            .get(self.pos)
            .ok_or_else(|| invalid("unexpected end"))?;
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn expect(&mut self, want: u8) -> Result<()> {
        self.skip_ws();
        if self.next()? == want {
            Ok(())
        } else {
            Err(invalid(format!(
                "expected '{}' at offset {}",
                char::from(want),
                self.pos - 1
            )))
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let digits = self
            .input
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| invalid("bad \\u escape"))?;
        let mut v = 0u32;
        for &b in digits {
            let d = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return Err(invalid("bad \\u escape")),
            };
            v = (v << 4) | u32::from(d);
        }
        self.pos += 4;
        Ok(v)
    }

    /// The character of one escape; the backslash is already consumed.
    fn escape(&mut self) -> Result<char> {
        Ok(match self.next()? {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let hi = self.hex4()?;
                let code = if (0xd800..0xdc00).contains(&hi) {
                    if self.input.get(self.pos..self.pos + 2) != Some(b"\\u") {
                        return Err(invalid("lone high surrogate"));
                    }
                    self.pos += 2;
                    let lo = self.hex4()?;
                    if !(0xdc00..0xe000).contains(&lo) {
                        return Err(invalid("bad low surrogate"));
                    }
                    0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                } else {
                    hi
                };
                char::from_u32(code).ok_or_else(|| invalid("lone low surrogate"))?
            }
            _ => return Err(invalid("bad escape")),
        })
    }

    /// A string; the opening quote is already consumed. Runs between escapes are copied whole
    /// from the already-checked text: per-byte work was most of the parse time
    /// (`tests/perf.rs`).
    fn string(&mut self) -> Result<String> {
        let mut out = String::new();
        loop {
            let start = self.pos;
            let run = plain_run(&self.input[start..]).ok_or_else(|| invalid("unexpected end"))?;
            self.pos += run;
            // Both ends sit next to an ASCII byte, so they are character boundaries.
            let plain = self
                .text
                .get(start..self.pos)
                .ok_or_else(|| invalid("not UTF-8"))?;
            match self.input[self.pos] {
                b'"' => {
                    self.pos += 1;
                    if out.is_empty() {
                        return Ok(plain.to_owned());
                    }
                    out.push_str(plain);
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    out.push_str(plain);
                    let c = self.escape()?;
                    out.push(c);
                }
                _ => return Err(invalid("control character in string")),
            }
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<()> {
        if self.input.get(self.pos..self.pos + word.len()) == Some(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(invalid("bad literal"))
        }
    }

    fn value(&mut self) -> Result<Json> {
        self.skip_ws();
        match self.input.get(self.pos) {
            Some(b'"') => {
                self.pos += 1;
                self.string().map(Json::Str)
            }
            Some(b't') => self.literal(b"true").map(|()| Json::Bool(true)),
            Some(b'f') => self.literal(b"false").map(|()| Json::Bool(false)),
            _ => Err(invalid(format!(
                "only string and boolean values are accepted (offset {})",
                self.pos
            ))),
        }
    }

    /// An integer: `-?(0|[1-9][0-9]*)` in the i64 range. A fraction or exponent is refused,
    /// not rounded.
    fn int(&mut self) -> Result<i64> {
        let neg = self.input.get(self.pos) == Some(&b'-');
        if neg {
            self.pos += 1;
        }
        let start = self.pos;
        let mut n: i64 = 0;
        while let Some(b @ b'0'..=b'9') = self.input.get(self.pos).copied() {
            if self.pos > start && self.input[start] == b'0' {
                return Err(invalid("leading zero"));
            }
            let d = i64::from(b - b'0');
            // Accumulating negatively reaches i64::MIN too.
            n = n
                .checked_mul(10)
                .and_then(|n| {
                    if neg {
                        n.checked_sub(d)
                    } else {
                        n.checked_add(d)
                    }
                })
                .ok_or_else(|| invalid("integer out of range"))?;
            self.pos += 1;
        }
        if self.pos == start {
            return Err(invalid("bad number"));
        }
        if let Some(b'.' | b'e' | b'E') = self.input.get(self.pos) {
            return Err(invalid("only integers are accepted"));
        }
        Ok(n)
    }

    /// Any value; `depth` is the nesting of the container it sits in.
    fn any(&mut self, depth: usize) -> Result<Value> {
        self.skip_ws();
        match self.input.get(self.pos) {
            Some(b'"') => {
                self.pos += 1;
                self.string().map(Value::Str)
            }
            Some(b't') => self.literal(b"true").map(|()| Value::Bool(true)),
            Some(b'f') => self.literal(b"false").map(|()| Value::Bool(false)),
            Some(b'n') => self.literal(b"null").map(|()| Value::Null),
            Some(b'-' | b'0'..=b'9') => self.int().map(Value::Int),
            Some(b'[') => {
                if depth >= MAX_DEPTH {
                    return Err(invalid("nested too deep"));
                }
                self.pos += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.input.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    if items.len() == MAX_ITEMS {
                        return Err(invalid("too many items"));
                    }
                    self.count_value()?;
                    items.push(self.any(depth + 1)?);
                    self.skip_ws();
                    match self.next()? {
                        b',' => {}
                        b']' => return Ok(Value::Arr(items)),
                        _ => return Err(invalid("expected ',' or ']'")),
                    }
                }
            }
            Some(b'{') => {
                if depth >= MAX_DEPTH {
                    return Err(invalid("nested too deep"));
                }
                self.members(depth + 1, |_, e| e).map(Value::Obj)
            }
            _ => Err(invalid(format!("bad value at offset {}", self.pos))),
        }
    }

    /// An object's members, `{` included; `wrap` adds context (the key) to a value's error.
    fn members(
        &mut self,
        depth: usize,
        wrap: impl Fn(&str, Error) -> Error,
    ) -> Result<Vec<(String, Value)>> {
        let mut fields: Vec<(String, Value)> = Vec::new();
        let mut keys = Keys::default();
        self.expect(b'{')?;
        self.skip_ws();
        if self.input.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(fields);
        }
        loop {
            if fields.len() == MAX_ITEMS {
                return Err(invalid("too many keys"));
            }
            self.count_value()?;
            self.expect(b'"')?;
            let key = self.string()?;
            if key.len() > MAX_KEY {
                return Err(invalid("key too long"));
            }
            if keys.duplicate(&fields, &key) {
                return Err(wrap(&key, invalid(format!("duplicate key {key:?}"))));
            }
            self.expect(b':').map_err(|e| wrap(&key, e))?;
            let value = self.any(depth).map_err(|e| wrap(&key, e))?;
            fields.push((key, value));
            self.skip_ws();
            match self.next()? {
                b',' => {}
                b'}' => {
                    if let Some(key) = keys.repeated(&fields) {
                        return Err(wrap(key, invalid(format!("duplicate key {key:?}"))));
                    }
                    return Ok(fields);
                }
                _ => return Err(invalid("expected ',' or '}'")),
            }
        }
    }

    fn object(mut self) -> Result<Vec<(String, Json)>> {
        let mut fields: Vec<(String, Json)> = Vec::new();
        let mut keys = Keys::default();
        self.expect(b'{')?;
        self.skip_ws();
        if self.input.get(self.pos) == Some(&b'}') {
            self.pos += 1;
        } else {
            loop {
                self.expect(b'"')?;
                let key = self.string()?;
                if keys.duplicate(&fields, &key) {
                    return Err(invalid(format!("duplicate key {key:?}")));
                }
                self.expect(b':')?;
                let value = self.value()?;
                fields.push((key, value));
                self.skip_ws();
                match self.next()? {
                    b',' => {}
                    b'}' => {
                        if let Some(key) = keys.repeated(&fields) {
                            return Err(invalid(format!("duplicate key {key:?}")));
                        }
                        break;
                    }
                    _ => return Err(invalid("expected ',' or '}'")),
                }
            }
        }
        self.skip_ws();
        if self.pos != self.input.len() {
            return Err(invalid("trailing data"));
        }
        Ok(fields)
    }
}

/// Parses `input` as one flat object, in key order.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] when it is longer than `max_len`, not UTF-8, or not strict JSON
/// of the accepted shape.
pub fn parse_object(input: &[u8], max_len: usize) -> Result<Vec<(String, Json)>> {
    if input.len() > max_len {
        return Err(invalid(format!("{} bytes is too long", input.len())));
    }
    let text = std::str::from_utf8(input).map_err(|_| invalid("not UTF-8"))?;
    Parser {
        text,
        input,
        pos: 0,
        values: 0,
    }
    .object()
}

/// Parses `input` as one object of any values but floats, in key order. An error inside a
/// member's value names that top-level key as its field.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] when it is longer than `max_len`, not UTF-8, not one object,
/// or breaks a rule above (float, out-of-range integer, leading zero, duplicate key, deeper
/// than [`MAX_DEPTH`], a container over [`MAX_ITEMS`], a key over [`MAX_KEY`]);
/// [`ErrorCode::TooComplex`] past [`MAX_VALUES`] values in all.
pub fn parse_document(input: &[u8], max_len: usize) -> Result<Vec<(String, Value)>> {
    if input.len() > max_len {
        return Err(invalid(format!("{} bytes is too long", input.len())));
    }
    let text = std::str::from_utf8(input).map_err(|_| invalid("not UTF-8"))?;
    let mut p = Parser {
        text,
        input,
        pos: 0,
        values: 0,
    };
    let fields = p.members(1, |key, e| e.at(key))?;
    p.skip_ws();
    if p.pos != input.len() {
        return Err(invalid("trailing data"));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::{MAX_DEPTH, MAX_ITEMS, MAX_VALUES, SCAN_KEYS, Value, parse_document, parse_object};
    use crate::testutil::Prng;
    use plumblog_model::ErrorCode;

    type Doc = Result<Vec<(String, Value)>, (String, Option<String>)>;

    fn doc(s: &str) -> Doc {
        parse_document(s.as_bytes(), 1 << 20).map_err(|e| (e.detail, e.field))
    }

    #[test]
    fn document_values() {
        let v = doc(r#" {"a":[1,-2,0,null,true,"x",{"b":[]}],"n":-9223372036854775808,"m":9223372036854775807} "#)
            .unwrap();
        assert_eq!(
            v[0].1,
            Value::Arr(vec![
                Value::Int(1),
                Value::Int(-2),
                Value::Int(0),
                Value::Null,
                Value::Bool(true),
                Value::Str("x".into()),
                Value::Obj(vec![("b".into(), Value::Arr(vec![]))]),
            ])
        );
        assert_eq!(v[1].1, Value::Int(i64::MIN));
        assert_eq!(v[2].1, Value::Int(i64::MAX));
        assert_eq!(doc("{}").unwrap(), vec![]);
    }

    #[test]
    fn document_refusals_name_the_top_level_field() {
        for (input, field) in [
            (r#"{"a":1.5}"#, Some("a")),
            (r#"{"a":1e3}"#, Some("a")),
            (r#"{"a":1E3}"#, Some("a")),
            (r#"{"a":-0.0}"#, Some("a")),
            (r#"{"a":01}"#, Some("a")),
            (r#"{"a":-01}"#, Some("a")),
            (r#"{"a":9223372036854775808}"#, Some("a")),
            (r#"{"a":-9223372036854775809}"#, Some("a")),
            (r#"{"a":99999999999999999999999999}"#, Some("a")),
            (r#"{"a":-}"#, Some("a")),
            (r#"{"a":+1}"#, Some("a")),
            (r#"{"a":NaN}"#, Some("a")),
            (r#"{"a":[1,]}"#, Some("a")),
            (r#"{"a":{"b":1,"b":2}}"#, Some("a")),
            (r#"{"a":"x","a":"y"}"#, Some("a")),
            (r#"{"a":nul}"#, Some("a")),
            (r#"{"a":1,}"#, None),
            ("[1]", None),
            ("1", None),
            ("", None),
            (r#"{"a":1} x"#, None),
        ] {
            let (_, got) = doc(input).expect_err(input);
            assert_eq!(got.as_deref(), field, "{input}");
        }
        let long_key = format!(r#"{{"{}":1}}"#, "k".repeat(65));
        assert_eq!(doc(&long_key).unwrap_err().1, None);
        assert!(parse_document(b"{\"a\":\"\xff\"}", 100).is_err());
        assert!(parse_document(b"{}", 1).is_err());
    }

    #[test]
    fn document_limits() {
        let nest = |n: usize| format!(r#"{{"a":{}1{}}}"#, "[".repeat(n), "]".repeat(n));
        assert!(doc(&nest(MAX_DEPTH - 1)).is_ok());
        assert_eq!(doc(&nest(MAX_DEPTH)).unwrap_err().1.as_deref(), Some("a"));
        // Far past the cap: refused at the cap, no stack overflow.
        assert!(doc(&nest(100_000)).is_err());
        let objs = format!(
            r#"{{"a":{}1{}}}"#,
            r#"{"a":"#.repeat(100_000),
            "}".repeat(100_000)
        );
        assert!(doc(&objs).is_err());
        let items = |n: usize| format!(r#"{{"a":[{}]}}"#, vec!["1"; n].join(","));
        assert!(doc(&items(MAX_ITEMS)).is_ok());
        assert!(doc(&items(MAX_ITEMS + 1)).is_err());
    }

    /// Values are counted at every depth, members and items alike, up to the cap exactly.
    #[test]
    fn document_value_cap() {
        // `{"a":[[...],...]}`: 1 member, `arrays` items, the rest ints in them.
        let doc_of = |total: usize| {
            let arrays = total.div_ceil(MAX_ITEMS + 1);
            let mut ints = total - 1 - arrays;
            let mut parts = Vec::new();
            for _ in 0..arrays {
                let n = ints.min(MAX_ITEMS);
                ints -= n;
                parts.push(format!("[{}]", vec!["1"; n].join(",")));
            }
            format!(r#"{{"a":[{}]}}"#, parts.join(","))
        };
        assert!(doc(&doc_of(MAX_VALUES)).is_ok());
        let over = doc_of(MAX_VALUES + 1);
        let err = parse_document(over.as_bytes(), 1 << 20).unwrap_err();
        assert_eq!(err.code, ErrorCode::TooComplex, "{}", err.detail);
        assert_eq!(err.field.as_deref(), Some("a"));
        // Every other refusal stays `input.invalid`.
        let e = parse_document(br#"{"a":[1,]}"#, 100).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        // Object members count too, and the cap holds across top-level members.
        let objects = (0..5)
            .map(|o| {
                let m: Vec<_> = (0..900).map(|i| format!(r#""k{i}":{o}"#)).collect();
                format!(r#""o{o}":{{{}}}"#, m.join(","))
            })
            .collect::<Vec<_>>();
        assert!(doc(&format!("{{{}}}", objects[..4].join(","))).is_ok());
        assert_eq!(
            doc(&format!("{{{}}}", objects.join(",")))
                .unwrap_err()
                .1
                .as_deref(),
            Some("o4")
        );
    }

    /// Duplicates are found at any position, before and past the plain-scan size, escaped or not.
    #[test]
    fn duplicates_in_large_objects() {
        let keys = |n: usize| (0..n).map(|i| format!(r#""k{i}":1"#)).collect::<Vec<_>>();
        for n in [2, SCAN_KEYS, SCAN_KEYS + 1, MAX_ITEMS - 1] {
            for dup in [0, n / 2, n - 1] {
                let mut k = keys(n);
                k.push(format!(r#""k{dup}":2"#));
                let top = format!("{{{}}}", k.join(","));
                assert_eq!(
                    doc(&top).unwrap_err().1,
                    Some(format!("k{dup}")),
                    "{n} {dup}"
                );
                let nested = format!(r#"{{"a":{top}}}"#);
                assert_eq!(doc(&nested).unwrap_err().1.as_deref(), Some("a"));
                // The same key written with an escape is the same key.
                let escaped =
                    top.replace(&format!(r#""k{dup}":2"#), &format!(r#""\u006b{dup}":2"#));
                assert!(doc(&escaped).is_err(), "{n} {dup}");
                let flat = top.replace(":1", r#":"x""#).replace(":2", r#":"y""#);
                assert!(parse_object(flat.as_bytes(), 1 << 20).is_err(), "{n} {dup}");
            }
            assert_eq!(doc(&format!("{{{}}}", keys(n).join(","))).unwrap().len(), n);
        }
    }

    /// Truncations and random mutations of a valid document fail cleanly or parse; never panic.
    #[test]
    fn document_never_panics() {
        let base = br#"{"title":"T\u00e9 \ud83d\ude00","n":-42,"tags":[1,2,3],"o":{"x":[null,true,false]},"s":"a\"b\\c"}"#;
        assert!(parse_document(base, 1 << 16).is_ok());
        for cut in 0..base.len() {
            assert!(parse_document(&base[..cut], 1 << 16).is_err());
        }
        let mut rng = Prng(0x5eed);
        for _ in 0..20_000 {
            let mut m = base.to_vec();
            for _ in 0..=rng.below(4) {
                let at = rng.below(m.len());
                let alphabet = b"{}[]\",:-0123456789.eEtfn \\u";
                m[at] = alphabet[rng.below(alphabet.len())];
            }
            let _ = parse_document(&m, 1 << 16);
            let len = rng.below(64);
            let _ = parse_document(&rng.bytes(len), 1 << 16);
        }
    }

    /// The flat reader keeps its stricter subset: numbers, null, arrays and objects refused.
    #[test]
    fn flat_object_is_unchanged() {
        assert!(parse_object(br#"{"a":"x","b":true}"#, 100).is_ok());
        for bad in [
            &br#"{"a":1}"#[..],
            br#"{"a":null}"#,
            br#"{"a":[]}"#,
            br#"{"a":{}}"#,
        ] {
            assert!(parse_object(bad, 100).is_err(), "{bad:?}");
        }
    }
}
