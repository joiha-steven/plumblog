//! Theme runtime. minijinja rendering lands with its own issue; this fixes which template a
//! page uses and the fallback to the default theme.

/// Reader page kinds; each maps to one theme template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Home,
    Post,
    Search,
}

impl PageKind {
    #[must_use]
    pub const fn template(self) -> &'static str {
        match self {
            Self::Home => "home.html",
            Self::Post => "post.html",
            Self::Search => "search.html",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateSource {
    Theme,
    Default,
}

/// Themes may override only some templates; the rest come from the default theme
/// (docs/ARCHITECTURE.md, Themes), so a partial theme still renders every page.
#[must_use]
pub fn resolve(page: PageKind, theme_has: impl Fn(&str) -> bool) -> (TemplateSource, &'static str) {
    let name = page.template();
    if theme_has(name) {
        (TemplateSource::Theme, name)
    } else {
        (TemplateSource::Default, name)
    }
}

#[cfg(test)]
mod tests {
    use super::{PageKind, TemplateSource, resolve};

    #[test]
    fn missing_theme_template_falls_back_to_default() {
        let theme = |name: &str| name == "post.html";
        assert_eq!(
            resolve(PageKind::Post, theme),
            (TemplateSource::Theme, "post.html")
        );
        assert_eq!(
            resolve(PageKind::Home, theme),
            (TemplateSource::Default, "home.html")
        );
    }
}
