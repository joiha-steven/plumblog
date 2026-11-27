//! Plain-text helpers: heading ids, word counting, excerpt and reading time.

use unicode_normalization::UnicodeNormalization;

/// Anchor id for a heading. Letters of any script are kept (HTML ids allow them), so
/// Vietnamese headings stay readable in links instead of collapsing to dashes. Input is
/// NFC-normalised first: otherwise decomposed text (macOS input) loses its combining marks.
#[must_use]
pub fn heading_id(text: &str) -> String {
    let mut id = String::with_capacity(text.len());
    for c in text.nfc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() {
        id.push_str("section");
    }
    id
}

/// Scripts written without spaces between words; each character counts as one word, which
/// is the usual convention for reading time and keeps excerpts from swallowing a paragraph.
fn is_unspaced(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // Hiragana, Katakana
        | 0x3400..=0x4DBF   // CJK Extension A
        | 0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0xF900..=0xFAFF   // CJK Compatibility Ideographs
        | 0x20000..=0x2FFFF // CJK Extensions B-F
    )
}

/// Byte ranges of words in `s`: whitespace-separated runs, with each unspaced-script
/// character its own word.
fn words(s: &str) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
    let mut chars = s.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
            chars.next();
        }
        let (start, c) = chars.next()?;
        if is_unspaced(c) {
            return Some((start, start + c.len_utf8(), true));
        }
        let mut end = start + c.len_utf8();
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() || is_unspaced(c) {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        Some((start, end, false))
    })
}

/// Words per minute for spaced scripts, characters per minute for unspaced ones; common
/// adult silent-reading rates.
const WPM: usize = 230;
const CPM: usize = 500;

#[derive(Default)]
pub struct Counter {
    spaced: usize,
    unspaced: usize,
}

impl Counter {
    pub fn add(&mut self, s: &str) {
        for (_, _, unspaced) in words(s) {
            if unspaced {
                self.unspaced += 1;
            } else {
                self.spaced += 1;
            }
        }
    }

    pub fn reading_minutes(&self) -> u32 {
        if self.spaced + self.unspaced == 0 {
            return 0;
        }
        // Integer ceil of spaced/WPM + unspaced/CPM, computed over a common denominator.
        let num = self.spaced * CPM + self.unspaced * WPM;
        let minutes = num.div_ceil(WPM * CPM).max(1);
        u32::try_from(minutes).unwrap_or(u32::MAX)
    }
}

/// First `max` words of `s` with whitespace collapsed, plus an ellipsis when cut.
pub fn excerpt(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut prev_end: Option<usize> = None;
    for (taken, (start, end, _)) in words(s).enumerate() {
        if taken == max {
            // Trailing separators read badly before an ellipsis ("word,...").
            let trimmed = out.trim_end_matches([',', ';', ':', '-']).len();
            out.truncate(trimmed);
            out.push('\u{2026}');
            return out;
        }
        if let Some(p) = prev_end {
            // Keep "no space" between adjacent unspaced characters; collapse other gaps.
            if p != start {
                out.push(' ');
            }
        }
        out.push_str(&s[start..end]);
        prev_end = Some(end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Counter, excerpt, heading_id};

    #[test]
    fn heading_ids_are_stable_and_keep_letters() {
        assert_eq!(heading_id("Hello, World!"), "hello-world");
        assert_eq!(heading_id("  Rust  &  wasm  "), "rust-wasm");
        assert_eq!(
            heading_id("Ti\u{1ebf}ng Vi\u{1ec7}t"),
            "ti\u{1ebf}ng-vi\u{1ec7}t"
        );
        // NFD "Ti\u{1ebf}ng": e + circumflex + acute as separate code points.
        assert_eq!(
            heading_id("Tie\u{302}\u{301}ng"),
            heading_id("Ti\u{1ebf}ng")
        );
        assert_eq!(heading_id("!!!"), "section");
    }

    #[test]
    fn excerpt_cuts_at_words_in_every_script() {
        assert_eq!(excerpt("one two  three\nfour", 3), "one two three\u{2026}");
        assert_eq!(excerpt("one two", 3), "one two");
        assert_eq!(excerpt("one, two", 1), "one\u{2026}");
        // Vietnamese syllables are space separated; diacritics never split.
        assert_eq!(
            excerpt("Vi\u{1ec7}t Nam \u{111}\u{1eb9}p l\u{1eaf}m", 2),
            "Vi\u{1ec7}t Nam\u{2026}"
        );
        // Chinese: one character per word, no spaces inserted.
        assert_eq!(
            excerpt("\u{4e2d}\u{6587}\u{535a}\u{5ba2}", 2),
            "\u{4e2d}\u{6587}\u{2026}"
        );
        assert_eq!(excerpt("", 5), "");
    }

    #[test]
    fn reading_time_rounds_up_and_is_zero_only_when_empty() {
        let mut c = Counter::default();
        assert_eq!(c.reading_minutes(), 0);
        c.add("word");
        assert_eq!(c.reading_minutes(), 1);
        c.add(&"w ".repeat(460));
        assert_eq!(c.reading_minutes(), 3);
        let mut cjk = Counter::default();
        cjk.add(&"\u{5b57}".repeat(1000));
        assert_eq!(cjk.reading_minutes(), 2);
    }
}
