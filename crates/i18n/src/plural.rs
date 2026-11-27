//! CLDR plural rules for whole numbers (counts of posts, minutes, comments).
//!
//! To add a language: find its cardinal rules at
//! <https://www.unicode.org/cldr/charts/latest/supplemental/language_plural_rules.html>,
//! add a match arm in `plural` for the integer (`i`, `v = 0`) cases and list its categories
//! in `categories`. The locale check then requires every plural message in that language's
//! file to have exactly those forms.

/// All six CLDR plural categories, so adding a language never changes this API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plural {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl Plural {
    /// The key used in locale files (`one`, `few`...).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "zero" => Self::Zero,
            "one" => Self::One,
            "two" => Self::Two,
            "few" => Self::Few,
            "many" => Self::Many,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// Base language of a tag: `pt-BR` -> `pt`, `zh_Hant` -> `zh`.
fn base(lang: &str) -> &str {
    lang.split(['-', '_']).next().unwrap_or(lang)
}

/// Plural category for the whole number `n` in `lang`. Unknown languages use English
/// rules, which at worst shows "1 posts"-style text rather than nothing.
#[must_use]
pub fn plural(lang: &str, n: u64) -> Plural {
    use Plural::{Few, Many, One, Other, Two, Zero};
    let (n10, n100) = (n % 10, n % 100);
    match base(lang) {
        // No grammatical plural.
        "vi" | "zh" | "ja" | "ko" | "th" | "id" | "ms" | "lo" | "km" | "my" => Other,
        "ru" | "uk" | "be" => match (n10, n100) {
            (1, 11) | (5..=9 | 0, _) | (_, 12..=14) => Many,
            (1, _) => One,
            _ => Few,
        },
        "pl" => match (n, n10, n100) {
            (1, _, _) => One,
            (_, 2..=4, 12..=14) => Many,
            (_, 2..=4, _) => Few,
            _ => Many,
        },
        "cs" | "sk" => match n {
            1 => One,
            2..=4 => Few,
            _ => Other,
        },
        "ar" => match (n, n100) {
            (0, _) => Zero,
            (1, _) => One,
            (2, _) => Two,
            (_, 3..=10) => Few,
            (_, 11..=99) => Many,
            _ => Other,
        },
        // French and Brazilian Portuguese count 0 as singular.
        "fr" => {
            if n <= 1 {
                One
            } else {
                Other
            }
        }
        "pt" if lang.ends_with("BR") || lang == "pt" => {
            if n <= 1 {
                One
            } else {
                Other
            }
        }
        _ => {
            if n == 1 {
                One
            } else {
                Other
            }
        }
    }
}

/// Forms a plural message must have in `lang`'s locale file.
#[must_use]
pub fn categories(lang: &str) -> &'static [Plural] {
    use Plural::{Few, Many, One, Other, Two, Zero};
    match base(lang) {
        "vi" | "zh" | "ja" | "ko" | "th" | "id" | "ms" | "lo" | "km" | "my" => &[Other],
        "ru" | "uk" | "be" | "pl" => &[One, Few, Many, Other],
        "cs" | "sk" => &[One, Few, Other],
        "ar" => &[Zero, One, Two, Few, Many, Other],
        _ => &[One, Other],
    }
}

#[cfg(test)]
mod tests {
    use super::{Plural, categories, plural};

    #[test]
    fn cldr_rules_for_shipped_and_sample_languages() {
        use Plural::{Few, Many, One, Other, Two, Zero};
        let en: Vec<_> = [0, 1, 2, 11].iter().map(|n| plural("en", *n)).collect();
        assert_eq!(en, [Other, One, Other, Other]);
        assert_eq!(plural("vi", 1), Other);
        let ru: Vec<_> = [1, 2, 4, 5, 11, 12, 21, 22, 25, 111, 0]
            .iter()
            .map(|n| plural("ru", *n))
            .collect();
        assert_eq!(
            ru,
            [One, Few, Few, Many, Many, Many, One, Few, Many, Many, Many]
        );
        let ar: Vec<_> = [0, 1, 2, 3, 11, 100]
            .iter()
            .map(|n| plural("ar", *n))
            .collect();
        assert_eq!(ar, [Zero, One, Two, Few, Many, Other]);
        assert_eq!(plural("fr", 0), One);
        assert_eq!(plural("pt-BR", 0), One);
        assert_eq!(plural("de", 0), Other);
        assert_eq!(plural("unknown", 1), One);
        // Every category a rule can return is one the language's files must provide.
        for lang in ["en", "vi", "ru", "pl", "cs", "ar", "fr", "pt-BR"] {
            for n in 0..=200 {
                assert!(categories(lang).contains(&plural(lang, n)), "{lang} {n}");
            }
        }
    }
}
