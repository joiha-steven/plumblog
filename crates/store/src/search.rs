//! Search text helpers shared by the D1 and in-memory stores. No I/O.
//!
//! D1 indexes with FTS5 `unicode61 remove_diacritics 2`, which drops tone and horn marks
//! (Vietnamese `d\u{1b0}\u{1edd}ng` -> `duong`) but keeps d-stroke as its own letter, so the app folds d-stroke itself
//! before indexing and querying. The in-memory matcher repeats what FTS5 does so both stores
//! give the same answers.

use unicode_normalization::UnicodeNormalization;

/// Most words a query keeps; more add cost without narrowing results meaningfully.
pub const MAX_QUERY_WORDS: usize = 8;

/// Replaces d-stroke (U+0111, U+0110) with `d`/`D` and nothing else: FTS5 folds the rest.
/// Apply to indexed text and queries alike.
#[must_use]
pub fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{0111}' => 'd',
            '\u{0110}' => 'D',
            c => c,
        })
        .collect()
}

/// Words of a query as typed (folded, accents kept): runs of letters and digits, at most
/// [`MAX_QUERY_WORDS`]. NFC first so text typed in decomposed form (macOS) splits the same.
#[must_use]
pub fn query_words(query: &str) -> Vec<String> {
    words(&fold(&query.nfc().collect::<String>()))
        .take(MAX_QUERY_WORDS)
        .map(str::to_owned)
        .collect()
}

/// An FTS5 MATCH expression for user text: every word as a quoted prefix term (`"word"*`),
/// space-separated (implicit AND). Words hold only letters and digits, so quotes, `*`, `-`,
/// `NEAR`, `OR` and other FTS5 syntax in user text become plain words or vanish and can
/// never error or change meaning. `None` if there is no word.
#[must_use]
pub fn to_fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query_words(query)
        .iter()
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// What FTS5 compares: lowercase with combining marks (U+0300..U+036F) removed after NFD.
#[must_use]
pub fn normalize(word: &str) -> String {
    word.to_lowercase()
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .collect()
}

/// Normalized tokens of indexed text, as the in-memory index sees them.
#[must_use]
pub fn tokens(text: &str) -> Vec<String> {
    let text = fold(&text.nfc().collect::<String>());
    words(&text).map(normalize).collect()
}

/// In-memory equivalent of MATCH `to_fts_query(query)` over a row of `(title, body)`:
/// every query word is a prefix of some token of either column. False if no word.
#[must_use]
pub fn matches(query: &str, title: &str, body: &str) -> bool {
    let wanted: Vec<String> = query_words(query).iter().map(|w| normalize(w)).collect();
    if wanted.is_empty() {
        return false;
    }
    let mut have = tokens(title);
    have.extend(tokens(body));
    wanted
        .iter()
        .all(|w| have.iter().any(|t| t.starts_with(w.as_str())))
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

#[cfg(test)]
mod tests {
    // Vietnamese is escaped (code must be ASCII): Da Lat = "\u{0110}\u{00E0} L\u{1EA1}t",
    // duong = "d\u{01B0}\u{1EDD}ng", NGUOI = "NG\u{01AF}\u{1EDC}I", den = "\u{0111}\u{1EBF}n".
    use super::{fold, matches, normalize, query_words, to_fts_query};

    #[test]
    fn fold_only_touches_d_stroke() {
        assert_eq!(
            fold("\u{0110}\u{00E0} L\u{1EA1}t \u{0111}\u{01B0}\u{1EDD}ng"),
            "D\u{00E0} L\u{1EA1}t d\u{01B0}\u{1EDD}ng"
        );
        assert_eq!(fold("NG\u{01AF}\u{1EDC}I"), "NG\u{01AF}\u{1EDC}I");
        assert_eq!(fold(""), "");
    }

    #[test]
    fn fts_query_quotes_every_word_as_prefix() {
        assert_eq!(
            to_fts_query("hello world").as_deref(),
            Some("\"hello\"* \"world\"*")
        );
        assert_eq!(
            to_fts_query("\u{0110}\u{00E0} L\u{1EA1}t").as_deref(),
            Some("\"D\u{00E0}\"* \"L\u{1EA1}t\"*")
        );
    }

    #[test]
    fn fts_query_neutralises_syntax() {
        assert_eq!(
            to_fts_query("\"a\" OR b* -c NEAR(").as_deref(),
            Some("\"a\"* \"OR\"* \"b\"* \"c\"* \"NEAR\"*")
        );
        for q in ["", "   ", "\"\"", "*-()^:+", "\u{0300}"] {
            assert_eq!(to_fts_query(q), None, "{q:?}");
        }
    }

    #[test]
    fn fts_query_keeps_at_most_eight_words() {
        let q = to_fts_query("1 2 3 4 5 6 7 8 9 10").unwrap();
        assert_eq!(q.matches('*').count(), 8);
        assert!(!q.contains("\"9\""));
    }

    #[test]
    fn decomposed_input_splits_like_composed() {
        let nfd = "du\u{031B}o\u{031B}\u{0300}ng"; // duong with horn and grave, typed decomposed
        assert_eq!(query_words(nfd), query_words("d\u{01B0}\u{1EDD}ng"));
        assert_eq!(normalize("NG\u{01AF}\u{1EDC}I"), "nguoi");
    }

    #[test]
    fn memory_match_ignores_accents_case_and_d_stroke() {
        let body = "Con d\u{01B0}\u{1EDD}ng \u{0111}\u{1EBF}n \u{0110}\u{00E0} L\u{1EA1}t c\u{1EE7}a NG\u{01AF}\u{1EDC}I \u{0111}i xa";
        assert!(matches("duong", "", body));
        assert!(matches("\u{0111}\u{01B0}\u{1EDD}ng", "", body));
        assert!(matches("da lat", "", body));
        assert!(matches("nguoi", "", body));
        assert!(matches("ng\u{01B0}", "", body), "prefix");
        assert!(
            matches("lat", "\u{0110}\u{00E0} L\u{1EA1}t", ""),
            "title counts"
        );
        assert!(
            matches("lat con", "\u{0110}\u{00E0} L\u{1EA1}t", body),
            "words may span columns"
        );
        assert!(!matches("uong", "", body), "prefix, not infix");
        assert!(!matches("duong hanoi", "", body), "every word must match");
        assert!(!matches("", "", body));
        assert!(
            matches("\"con\" d\u{01B0}* -den (", "", body),
            "syntax is ignored"
        );
        assert!(
            !matches("con OR xa", "", body),
            "OR is a word, not an operator"
        );
    }
}
