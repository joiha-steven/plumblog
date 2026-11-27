//! Plain-text helpers: heading ids, word counting, excerpt and reading time.

use std::borrow::Cow;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

/// NFC form of `s`, borrowing when it already is (the common case, checked cheaply).
pub fn nfc(s: &str) -> Cow<'_, str> {
    if is_nfc_quick(s.chars()) == IsNormalized::Yes {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.nfc().collect())
    }
}

/// Anchor id for a heading. Letters of any script are kept (HTML ids allow them), so
/// Vietnamese headings stay readable in links instead of collapsing to dashes. Input is
/// NFC-normalised first: otherwise decomposed text (macOS input) loses its combining marks.
#[must_use]
pub fn heading_id(text: &str) -> String {
    let mut id = String::with_capacity(text.len());
    for c in text.nfc().flat_map(char::to_lowercase) {
        if is_latin_mark(c) {
            // Left over only where lowercasing has no precomposed form ("\u{130}" -> "i" +
            // dot above); dropping it keeps "\u{130}stanbul" as one word.
        } else if c.is_alphanumeric() || (is_mark(c) && !id.is_empty() && !id.ends_with('-')) {
            // Script marks (Thai tones, kana voicing) belong to the letter before them.
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

/// Generic combining diacritics (Latin, Greek, Cyrillic...).
fn is_latin_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Combining marks that never start a word: generic diacritics plus the vowel and tone
/// signs of the unspaced scripts below and the kana voicing marks.
fn is_mark(c: char) -> bool {
    is_latin_mark(c)
        || matches!(c as u32,
            0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E           // Thai
            | 0x0EB1 | 0x0EB4..=0x0EBC | 0x0EC8..=0x0ECE         // Lao
            | 0x102B..=0x103E | 0x1056..=0x1059 | 0x105E..=0x1060
            | 0x1062..=0x1064 | 0x1067..=0x106D | 0x1071..=0x1074
            | 0x1082..=0x108D | 0x108F | 0x109A..=0x109D         // Myanmar
            | 0x17B4..=0x17D3 | 0x17DD                           // Khmer
            | 0x3099..=0x309A                                    // kana voicing
        )
}

/// How a script separates words.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Spaced,
    /// Chinese and Japanese: each character is a word (the usual reading-time convention).
    Cjk,
    /// Thai, Lao, Khmer, Myanmar: no spaces between words and no dictionary here, so each
    /// character (with its marks) counts as a unit; excerpts are cut by characters.
    SouthEastAsian,
}

fn kind(c: char) -> Kind {
    match c as u32 {
        0x3040..=0x3098 | 0x309B..=0x30FF   // Hiragana, Katakana (not the voicing marks)
        | 0x3400..=0x4DBF                   // CJK Extension A
        | 0x4E00..=0x9FFF                   // CJK Unified Ideographs
        | 0xF900..=0xFAFF                   // CJK Compatibility Ideographs
        | 0x20000..=0x2FFFF => Kind::Cjk,   // CJK Extensions B-F
        0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF => Kind::SouthEastAsian,
        _ => Kind::Spaced,
    }
}

/// Byte ranges of words in `s`: whitespace-separated runs, with each unspaced-script
/// character (plus its marks) its own word.
fn words(s: &str) -> impl Iterator<Item = (usize, usize, Kind)> + '_ {
    let mut chars = s.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
            chars.next();
        }
        let (start, c) = chars.next()?;
        let k = kind(c);
        let mut end = start + c.len_utf8();
        while let Some(&(i, c)) = chars.peek() {
            let joins = if k == Kind::Spaced {
                !c.is_whitespace() && kind(c) == Kind::Spaced
            } else {
                is_mark(c)
            };
            if !joins {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        Some((start, end, k))
    })
}

/// Words in `s` as reading time counts them: each CJK or Thai-like character is one.
#[must_use]
pub fn word_count(s: &str) -> usize {
    words(s).count()
}

/// Common adult silent-reading rates: words per minute for spaced scripts, characters per
/// minute for CJK, and for Thai-like scripts (about 4-5 characters per word at ~220 wpm).
const WPM: usize = 230;
const CJK_CPM: usize = 500;
const SEA_CPM: usize = 1000;

#[derive(Default)]
pub struct Counter {
    spaced: usize,
    cjk: usize,
    sea: usize,
}

impl Counter {
    pub fn add(&mut self, s: &str) {
        for (_, _, k) in words(s) {
            match k {
                Kind::Spaced => self.spaced += 1,
                Kind::Cjk => self.cjk += 1,
                Kind::SouthEastAsian => self.sea += 1,
            }
        }
    }

    pub fn reading_minutes(&self) -> u32 {
        if self.spaced + self.cjk + self.sea == 0 {
            return 0;
        }
        // Integer ceil of the sum of fractions, over the common denominator WPM * SEA_CPM
        // (CJK_CPM divides SEA_CPM).
        let den = WPM * SEA_CPM;
        let num = self.spaced * SEA_CPM + self.cjk * WPM * (SEA_CPM / CJK_CPM) + self.sea * WPM;
        let minutes = num.div_ceil(den).max(1);
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
        assert_eq!(heading_id("\u{130}stanbul"), "istanbul");
        // Thai tone marks stay with their letter instead of splitting the id.
        assert_eq!(heading_id("\u{e44}\u{e17}\u{e22}"), "\u{e44}\u{e17}\u{e22}");
        assert_eq!(heading_id("\u{e01}\u{e48}\u{e32}"), "\u{e01}\u{e48}\u{e32}");
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
    fn unspaced_scripts_cut_by_character_with_marks_attached() {
        // Thai: 1,200 characters with no spaces still cut to 5 units.
        let thai = "\u{e20}\u{e32}\u{e29}\u{e32}\u{e44}\u{e17}\u{e22}".repeat(172);
        let cut = excerpt(&thai, 5);
        assert_eq!(cut, "\u{e20}\u{e32}\u{e29}\u{e32}\u{e44}\u{2026}");
        // A tone mark never becomes its own unit.
        assert_eq!(
            excerpt("\u{e01}\u{e48}\u{e32}", 1),
            "\u{e01}\u{e48}\u{2026}"
        );
        // Decomposed kana voicing (macOS NFD) stays on its letter.
        assert_eq!(
            excerpt("\u{304b}\u{3099}\u{304d}", 1),
            "\u{304b}\u{3099}\u{2026}"
        );
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
