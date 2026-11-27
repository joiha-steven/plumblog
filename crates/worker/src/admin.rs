//! The admin's HTML shell, web app manifest and icon (docs/ARCHITECTURE.md, Admin and editor).
//! Its scripts and stylesheets are static assets under `/admin/` (built from `web/admin/` by
//! `scripts/admin-build`), which Workers serve before this code runs; every other `/admin/*`
//! path is a client route and gets the shell.
//!
//! The shell carries the UI text for one language as JSON, so the first paint needs no
//! request. A strict CSP allows only same-origin scripts and styles: no theme or plugin code
//! ever runs here.

use crate::keys;
use crate::pages::core_text;
use crate::pages::source::{CORE_LOCALES, ReaderSource, SampleSource};
use plumblog_i18n::{Catalog, Key};
use worker::{Headers, Request, Response, Result};

/// Path prefix of every admin URL.
pub const PREFIX: &str = "/admin";
/// Cookie the language menu sets; it wins over `Accept-Language`.
const LANG_COOKIE: &str = "pb_lang";
/// Scripts and styles from this origin only. Inline `style` attributes are blocked too; the
/// client sets styles through the CSSOM (`el.style`), which CSP allows. `img-src blob:` is for
/// image previews before upload.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
    img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; worker-src 'self'; \
    manifest-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
    frame-ancestors 'none'";

/// Browser chrome color behind the installed app, matching `--bg` in web/admin/css/tokens.css.
const BG_LIGHT: &str = "#f3f2ef";
const BG_DARK: &str = "#1a1918";
/// The icon's plate: always dark, so the admin is told apart from the reader site's icon.
const PLATE: &str = "#1a1918";
/// Braun signal orange (`--signal`, dark theme), the admin's one accent.
const SIGNAL: &str = "#e0804b";

pub async fn serve(req: &Request) -> Result<Response> {
    let path = req.path();
    if path == PREFIX {
        let h = Headers::new();
        h.set("location", "/admin/")?;
        return Ok(Response::empty()?.with_status(301).with_headers(h));
    }
    // The sample settings until site settings move to D1, the same source reader pages use.
    let site = match SampleSource::get().settings().await {
        Ok(s) => s,
        Err(e) => {
            worker::console_error!("admin: site settings: {e}");
            return Response::error(core_text("en", keys::PAGE_FAILED), 500);
        }
    };
    let lang = pick_lang(
        req.headers().get("cookie")?.as_deref(),
        req.headers().get("accept-language")?.as_deref(),
        &site.lang,
    );
    // A missing file is a 404, never the shell: after a deploy an open tab may still import an
    // old chunk, and HTML served under a `.js` URL breaks the editor (nosniff) and would be
    // cached by the service worker as that chunk.
    if is_file(&path)
        && !matches!(
            path.as_str(),
            "/admin/manifest.webmanifest" | "/admin/icon.svg"
        )
    {
        let h = Headers::new();
        h.set("content-type", "text/plain; charset=utf-8")?;
        h.set("cache-control", "no-store")?;
        return Ok(
            Response::ok(text(&lang, keys::ADMIN_NOT_FOUND, &site.title))?
                .with_status(404)
                .with_headers(h),
        );
    }
    let (body, content_type) = match path.as_str() {
        "/admin/manifest.webmanifest" => (
            manifest(&lang, &site.title),
            "application/manifest+json; charset=utf-8",
        ),
        "/admin/icon.svg" => (icon(&site.title), "image/svg+xml; charset=utf-8"),
        _ => (shell(&lang, &site.title), "text/html; charset=utf-8"),
    };
    let h = Headers::new();
    h.set("content-type", content_type)?;
    // Small and language-dependent: revalidate every time rather than serve a stale shell.
    h.set("cache-control", "no-cache")?;
    h.set("vary", "cookie, accept-language")?;
    h.set("content-security-policy", CSP)?;
    h.set("x-content-type-options", "nosniff")?;
    h.set("referrer-policy", "same-origin")?;
    h.set("x-robots-tag", "noindex")?;
    Ok(Response::ok(body)?.with_headers(h))
}

/// Whether `path` names a file (its last segment has an extension, or it is under
/// `/admin/chunks/`) rather than a client route.
fn is_file(path: &str) -> bool {
    path.starts_with("/admin/chunks/")
        || path
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains('.'))
}

/// Languages the admin speaks: every core locale file.
fn langs() -> impl Iterator<Item = &'static str> {
    CORE_LOCALES.iter().map(|(l, _)| *l)
}

/// The cookie's language, else the first `Accept-Language` entry the admin speaks, else the
/// site language, else English.
fn pick_lang(cookie: Option<&str>, accept: Option<&str>, site_lang: &str) -> String {
    let speaks = |l: &str| langs().any(|x| x == l);
    let from_cookie = cookie.into_iter().flat_map(|c| c.split(';')).find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        (k == LANG_COOKIE && speaks(v)).then(|| v.to_owned())
    });
    let from_header = || {
        accept
            .into_iter()
            .flat_map(|a| a.split(','))
            .filter_map(|p| p.split(';').next())
            .map(|tag| {
                tag.trim()
                    .split('-')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
            })
            .find(|l| speaks(l))
    };
    from_cookie
        .or_else(from_header)
        .or_else(|| speaks(site_lang).then(|| site_lang.to_owned()))
        .unwrap_or_else(|| "en".to_owned())
}

/// What the client needs before its first paint besides its text: the language, the
/// languages on offer and the site name. The UI text is the admin bundle's own
/// (`/admin/i18n/<lang>.js`, built by scripts/admin-build), preloaded by the shell.
fn boot_json(lang: &str, site: &str) -> String {
    let langs: Vec<String> = langs().map(quote).collect();
    format!(
        "{{\"lang\":{},\"langs\":[{}],\"site\":{}}}",
        quote(lang),
        langs.join(","),
        quote(site),
    )
}

fn text(lang: &str, key: Key, site: &str) -> String {
    let all = |l: &str| {
        CORE_LOCALES
            .iter()
            .find(|(name, _)| *name == l)
            .and_then(|(_, src)| match plumblog_i18n::parse(src) {
                Ok(m) => Some(m),
                Err(e) => {
                    worker::console_error!("admin: locales/{l}.toml: {e}");
                    None
                }
            })
            .unwrap_or_default()
    };
    Catalog::new(all(lang), all("en"))
        .with_lang(lang)
        .t(key, &[("site", site.to_owned())])
}

/// The page every client route loads. Nothing in it depends on the route.
fn shell(lang: &str, site: &str) -> String {
    // `<` escaped so the JSON cannot close its script element.
    let boot = boot_json(lang, site).replace('<', "\\u003c");
    format!(
        "<!doctype html>\n<html lang=\"{lang}\" dir=\"ltr\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n\
<title>{title}</title>\n<meta name=\"robots\" content=\"noindex\">\n\
<meta name=\"color-scheme\" content=\"light dark\">\n\
<meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"{BG_LIGHT}\">\n\
<meta name=\"theme-color\" media=\"(prefers-color-scheme: dark)\" content=\"{BG_DARK}\">\n\
<link rel=\"manifest\" href=\"/admin/manifest.webmanifest\">\n\
<link rel=\"icon\" href=\"/admin/icon.svg\" type=\"image/svg+xml\">\n\
<link rel=\"stylesheet\" href=\"/admin/app.css\">\n\
<script type=\"module\" src=\"/admin/app.js\"></script>\n\
<link rel=\"modulepreload\" href=\"/admin/i18n/{lang}.js\">\n\
<script id=\"boot\" type=\"application/json\">{boot}</script>\n\
</head>\n<body>\n<div id=\"app\"></div>\n<noscript>{noscript}</noscript>\n</body>\n</html>\n",
        title = escape(&text(lang, keys::ADMIN_TITLE, site)),
        noscript = escape(&text(lang, keys::ADMIN_NOSCRIPT, site)),
    )
}

fn manifest(lang: &str, site: &str) -> String {
    let name = text(lang, keys::ADMIN_TITLE, site);
    format!(
        "{{\"name\":{name},\"short_name\":{name},\"lang\":{lang},\"start_url\":\"/admin/\",\
\"scope\":\"/admin/\",\"display\":\"standalone\",\"background_color\":\"{BG_DARK}\",\
\"theme_color\":\"{BG_DARK}\",\"icons\":[{{\"src\":\"/admin/icon.svg\",\"sizes\":\"any\",\
\"type\":\"image/svg+xml\",\"purpose\":\"any maskable\"}}]}}",
        name = quote(&name),
        lang = quote(lang),
    )
}

/// The default mark until a site icon can be uploaded: the site's first letter on a signal
/// disc, on a dark plate. The disc stays inside the maskable safe zone (80%).
fn icon(site: &str) -> String {
    let letter: String = site
        .trim()
        .chars()
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().collect());
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 512 512\">\
<rect width=\"512\" height=\"512\" fill=\"{PLATE}\"/>\
<circle cx=\"256\" cy=\"256\" r=\"150\" fill=\"{SIGNAL}\"/>\
<text x=\"256\" y=\"256\" dy=\".35em\" text-anchor=\"middle\" font-family=\"system-ui, sans-serif\" \
font-size=\"170\" font-weight=\"600\" fill=\"{PLATE}\">{}</text></svg>",
        escape(&letter)
    )
}

/// A JSON string literal.
fn quote(s: &str) -> String {
    plumblog_i18n::admin::json_string(s)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{boot_json, icon, is_file, manifest, pick_lang, shell};

    #[test]
    fn files_are_told_apart_from_client_routes() {
        for f in [
            "/admin/chunks/editor-OLD.js",
            "/admin/app.js",
            "/admin/app.css",
            "/admin/chunks/x",
            "/admin/a/b.map",
        ] {
            assert!(is_file(f), "{f}");
        }
        for r in [
            "/admin/",
            "/admin/posts",
            "/admin/posts/12",
            "/admin/settings/site",
        ] {
            assert!(!is_file(r), "{r}");
        }
    }

    #[test]
    fn language_comes_from_cookie_then_header_then_site() {
        assert_eq!(
            pick_lang(Some("a=1; pb_lang=vi"), Some("en-US"), "en"),
            "vi"
        );
        assert_eq!(
            pick_lang(Some("pb_lang=xx"), Some("fr, vi;q=0.8"), "en"),
            "vi"
        );
        assert_eq!(pick_lang(None, Some("fr-FR,de;q=0.5"), "vi"), "vi");
        assert_eq!(pick_lang(None, None, "fr"), "en");
    }

    #[test]
    fn the_shell_is_one_page_whose_json_cannot_break_out() {
        let html = shell("vi", "</script><b>\"x\"");
        assert!(html.starts_with("<!doctype html>\n<html lang=\"vi\""));
        assert!(
            !html.contains("</script><b>"),
            "site name escaped everywhere"
        );
        assert!(html.contains("<script type=\"module\" src=\"/admin/app.js\"></script>"));
        // No inline script or style for the CSP to allow: the boot block is data.
        assert_eq!(html.matches("<script").count(), 2);
        assert!(html.contains("<script id=\"boot\" type=\"application/json\">"));
        assert!(!html.contains("style="));
    }

    #[test]
    fn boot_json_has_the_language_but_no_text() {
        let vi = boot_json("vi", "S");
        assert_eq!(
            vi,
            "{\"lang\":\"vi\",\"langs\":[\"en\",\"vi\"],\"site\":\"S\"}"
        );
        assert!(
            shell("vi", "S").contains("<link rel=\"modulepreload\" href=\"/admin/i18n/vi.js\">")
        );
    }

    #[test]
    fn manifest_and_icon() {
        let m = manifest("en", "Plum \"Notes\"");
        assert!(m.contains("\"scope\":\"/admin/\""));
        assert!(m.contains("Plum \\\"Notes\\\""), "{m}");
        assert!(icon("plum").contains(">P</text>"));
        assert!(icon("<").contains(">&lt;</text>"));
        assert!(icon("").contains("></text>"));
    }
}
