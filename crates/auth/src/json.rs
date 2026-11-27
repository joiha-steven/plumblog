//! A tiny strict JSON reader: one top-level object whose values are strings or booleans, no
//! duplicate keys, UTF-8 only, size-capped by the caller. Anything else is refused rather than
//! guessed at. Used for `clientDataJSON` and for the Worker's API request bodies, which only
//! ever carry flat string/boolean fields.

use plumblog_model::{Error, ErrorCode, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Str(String),
    Bool(bool),
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("json: {}", detail.into()))
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.input.get(self.pos) {
            self.pos += 1;
        }
    }

    fn next(&mut self) -> Result<u8> {
        let b = *self
            .input
            .get(self.pos)
            .ok_or_else(|| invalid("unexpected end"))?;
        self.pos += 1;
        Ok(b)
    }

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
        let mut v = 0u32;
        for _ in 0..4 {
            let d = char::from(self.next()?)
                .to_digit(16)
                .ok_or_else(|| invalid("bad \\u escape"))?;
            v = (v << 4) | d;
        }
        Ok(v)
    }

    /// A string; the opening quote is already consumed. The input is known to be UTF-8, so
    /// copying non-escape bytes through keeps it UTF-8.
    fn string(&mut self) -> Result<String> {
        let mut out = Vec::new();
        loop {
            match self.next()? {
                b'"' => break,
                b'\\' => {
                    let c = match self.next()? {
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
                                if self.next()? != b'\\' || self.next()? != b'u' {
                                    return Err(invalid("lone high surrogate"));
                                }
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
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                b if b < 0x20 => return Err(invalid("control character in string")),
                b => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| invalid("not UTF-8"))
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

    fn object(mut self) -> Result<Vec<(String, Json)>> {
        let mut fields: Vec<(String, Json)> = Vec::new();
        self.expect(b'{')?;
        self.skip_ws();
        if self.input.get(self.pos) == Some(&b'}') {
            self.pos += 1;
        } else {
            loop {
                self.expect(b'"')?;
                let key = self.string()?;
                if fields.iter().any(|(k, _)| *k == key) {
                    return Err(invalid(format!("duplicate key {key:?}")));
                }
                self.expect(b':')?;
                let value = self.value()?;
                fields.push((key, value));
                self.skip_ws();
                match self.next()? {
                    b',' => {}
                    b'}' => break,
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
    std::str::from_utf8(input).map_err(|_| invalid("not UTF-8"))?;
    Parser { input, pos: 0 }.object()
}
