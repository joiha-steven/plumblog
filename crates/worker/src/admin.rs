//! The admin's HTML shell, web app manifest and icon (docs/ARCHITECTURE.md, Admin and editor).
//! Its scripts and stylesheets are static assets under `/admin/` (built from `web/admin/` by
//! `scripts/admin-build`), which Workers serve before this code runs; every other `/admin/*`
//! path is a client route and gets the shell.
//!
//! The shell carries the UI text for one language as JSON, so the first paint needs no
//! request. A strict CSP allows only same-origin scripts and styles: no theme or plugin code
//! ever runs here.

use crate::d1::D1Store;
use crate::keys;
use crate::pages::core_text;
use crate::pages::source::{ADMIN_PRELOAD, CORE_LOCALES, ReaderSource};
use plumblog_i18n::{Catalog, Key};
use worker::{Env, Headers, Request, Response, Result};

/// Path prefix of every admin URL.
pub const PREFIX: &str = "/admin";
/// Cookie the language menu sets; it wins over `Accept-Language`.
const LANG_COOKIE: &str = "pb_lang";
/// Scripts and styles from this origin only. Inline `style` attributes are blocked too; the
/// client sets styles through the CSSOM (`el.style`), which CSP allows. The one `<style>`
/// element allowed is the editor's (`CodeMirror` adds its base theme that way, see
/// `EditorView.cspNonce`), and only with this response's nonce. `img-src blob:` is for image
/// previews before upload.
///
/// The sign-in, setup and invite pages may also run Cloudflare Turnstile (its script and its
/// frame), which the server asks for after repeated failures. Only those pages: the client
/// always reaches them by a full page load, so no other admin page ever runs with it.
fn csp(nonce: &str, turnstile: bool) -> String {
    let (script, frame) = if turnstile {
        (
            format!("'self' {TURNSTILE_ORIGIN}"),
            format!(" frame-src {TURNSTILE_ORIGIN};"),
        )
    } else {
        ("'self'".to_owned(), String::new())
    };
    format!(
        "default-src 'self'; script-src {script}; style-src 'self' 'nonce-{nonce}'; \
    img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; worker-src 'self';{frame} \
    manifest-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
    frame-ancestors 'none'"
    )
}

/// Where Turnstile's script and challenge frame come from.
const TURNSTILE_ORIGIN: &str = "https://challenges.cloudflare.com";

/// The pages a signed-out visitor uses; the only ones whose CSP admits Turnstile.
/// At most one trailing slash, the same rule as the service worker's (`web/admin/src/sw.ts`):
/// a looser match here would hand the Turnstile policy to a path the service worker treats as
/// an ordinary page, and keep it as the offline shell.
fn is_auth_page(path: &str) -> bool {
    let p = path.strip_suffix('/').unwrap_or(path);
    matches!(p, "/admin/signin" | "/admin/setup" | "/admin/invite")
}

/// 16 bytes from the platform's cryptographic RNG, hex. A nonce must not be guessable, so
/// there is no fallback to a weaker source.
fn nonce() -> std::result::Result<String, String> {
    use plumblog_auth::Crypto;
    use std::fmt::Write;
    let mut buf = [0u8; 16];
    crate::webcrypto::WebCrypto::new()
        .and_then(|c| c.random_bytes(&mut buf))
        .map_err(|e| e.to_string())?;
    Ok(buf.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// Browser chrome color behind the installed app, matching `--bg` in web/admin/css/tokens.css.
const BG_LIGHT: &str = "#f3f2ef";
const BG_DARK: &str = "#1a1918";
/// The icon's plate: always dark, so the admin is told apart from the reader site's icon.
const PLATE: &str = "#1a1918";
/// Braun signal orange (`--signal`, dark theme), the admin's one accent.
const SIGNAL: &str = "#e0804b";

pub async fn serve(req: &Request, env: &Env) -> Result<Response> {
    let path = req.path();
    if path == PREFIX {
        let h = Headers::new();
        h.set("location", "/admin/")?;
        return Ok(Response::empty()?.with_status(301).with_headers(h));
    }
    // The site's own settings (1 query). The admin is how a broken site gets fixed, so an
    // unreadable store shows the shell with default settings rather than an error page.
    let site = match env.d1("DB") {
        Ok(d) => match crate::pages::db_source(&D1Store::new(d)).settings().await {
            Ok(s) => s,
            Err(e) => {
                worker::console_error!("admin: site settings: {e}");
                crate::pages::reader_db::parse_settings(&[])
            }
        },
        Err(e) => {
            worker::console_error!("admin: D1 binding DB: {e}");
            crate::pages::reader_db::parse_settings(&[])
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
    let nonce = match nonce() {
        Ok(n) => n,
        Err(e) => {
            worker::console_error!("admin: no random nonce: {e}");
            return Response::error(core_text("en", keys::PAGE_FAILED), 500);
        }
    };
    let (body, content_type) = match path.as_str() {
        "/admin/manifest.webmanifest" => (
            manifest(&lang, &site.title),
            "application/manifest+json; charset=utf-8",
        ),
        "/admin/icon.svg" => (icon(&site.title), "image/svg+xml; charset=utf-8"),
        _ => (
            shell(&lang, &site.title, &nonce, ADMIN_PRELOAD, uses_faces(&path)),
            "text/html; charset=utf-8",
        ),
    };
    let h = Headers::new();
    h.set("content-type", content_type)?;
    // Small and language-dependent: revalidate every time rather than serve a stale shell.
    h.set("cache-control", "no-cache")?;
    h.set("vary", "cookie, accept-language")?;
    h.set("content-security-policy", &csp(&nonce, is_auth_page(&path)))?;
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
/// languages on offer, the site name and the style nonce. The UI text is the admin bundle's
/// own (`/admin/i18n/<lang>.js`, built by scripts/admin-build), preloaded by the shell.
fn boot_json(lang: &str, site: &str, nonce: &str) -> String {
    let langs: Vec<String> = langs().map(quote).collect();
    format!(
        "{{\"lang\":{},\"langs\":[{}],\"site\":{},\"nonce\":{}}}",
        quote(lang),
        langs.join(","),
        quote(site),
        quote(nonce),
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

/// The page every client route loads. Only the CSP and `faces` (the editor and the sign-in
/// screens, the pages that use the reading face) depend on the route. `chunks` are the
/// entry's static imports (scripts/admin-build): named here, the browser fetches them beside
/// app.js instead of after it. Every page needs them all, signed in or not.
fn shell(lang: &str, site: &str, nonce: &str, chunks: &[&str], faces: bool) -> String {
    // `<` escaped so the JSON cannot close its script element.
    let boot = boot_json(lang, site, nonce).replace('<', "\\u003c");
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
{chunks}{fonts}<script id=\"boot\" type=\"application/json\">{boot}</script>\n\
</head>\n<body>\n<div id=\"app\"></div>\n<noscript>{noscript}</noscript>\n</body>\n</html>\n",
        chunks = chunk_preloads(chunks),
        fonts = if faces {
            font_preloads(lang)
        } else {
            String::new()
        },
        title = escape(&text(lang, keys::ADMIN_TITLE, site)),
        noscript = escape(&text(lang, keys::ADMIN_NOSCRIPT, site)),
    )
}

fn chunk_preloads(chunks: &[&str]) -> String {
    let mut out = String::new();
    for c in chunks {
        out.push_str("<link rel=\"modulepreload\" href=\"");
        out.push_str(c);
        out.push_str("\">\n");
    }
    out
}

/// Whether a page sets text in the writing faces: the editor and the sign-in story. Other
/// pages use system fonts, and preloading 50 KB of faces there only competes with the
/// scripts and data the first paint waits for (Lighthouse mobile LCP, 1.8 s against 1.2 s).
fn uses_faces(path: &str) -> bool {
    path.starts_with("/admin/posts/") || is_auth_page(path)
}

/// The writing column's faces use `font-display: optional` (scripts/admin-build), so a face
/// that misses the first paint is skipped rather than swapped in later, which would move every
/// line under it. Preloading the faces most text needs lets them make the first paint.
fn font_preloads(lang: &str) -> String {
    let mut files = vec!["inter-latin", "inter-display-latin"];
    if lang == "vi" {
        files.extend(["inter-vietnamese", "inter-display-vietnamese"]);
    }
    let mut out = String::new();
    for f in files {
        out.push_str("<link rel=\"preload\" href=\"/admin/fonts/");
        out.push_str(f);
        out.push_str(".woff2\" as=\"font\" type=\"font/woff2\" crossorigin>\n");
    }
    out
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
    use super::{
        boot_json, csp, icon, is_auth_page, is_file, manifest, pick_lang, shell, uses_faces,
    };

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
        let html = shell("vi", "</script><b>\"x\"", "n0", &[], true);
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
    fn the_shell_preloads_the_chunks_it_is_given() {
        let html = shell(
            "en",
            "S",
            "n0",
            &["/admin/chunks/a-1.js", "/admin/chunks/b-2.js"],
            false,
        );
        assert!(html.contains("<link rel=\"modulepreload\" href=\"/admin/chunks/a-1.js\">\n"));
        assert!(html.contains("<link rel=\"modulepreload\" href=\"/admin/chunks/b-2.js\">\n"));
        assert_eq!(
            html.matches("modulepreload").count(),
            3,
            "two chunks and the text"
        );
        assert_eq!(html.matches("<script").count(), 2, "preloads add no script");
    }

    #[test]
    fn the_shell_preloads_the_faces_its_language_needs() {
        let link = |f: &str| {
            format!(
                "<link rel=\"preload\" href=\"/admin/fonts/{f}.woff2\" as=\"font\" type=\"font/woff2\" crossorigin>"
            )
        };
        let en = shell("en", "S", "n0", &[], true);
        assert!(en.contains(&link("inter-latin")) && en.contains(&link("inter-display-latin")));
        assert!(!en.contains("vietnamese"));
        let vi = shell("vi", "S", "n0", &[], true);
        assert!(
            vi.contains(&link("inter-vietnamese"))
                && vi.contains(&link("inter-display-vietnamese"))
        );
        assert!(!shell("en", "S", "n0", &[], false).contains("fonts/"));
    }

    #[test]
    fn only_the_editor_and_sign_in_pages_preload_faces() {
        for p in [
            "/admin/posts/12",
            "/admin/posts/new",
            "/admin/signin",
            "/admin/setup/",
        ] {
            assert!(uses_faces(p), "{p}");
        }
        for p in [
            "/admin/",
            "/admin/pages",
            "/admin/media",
            "/admin/settings",
            "/admin/posts",
        ] {
            assert!(!uses_faces(p), "{p}");
        }
    }

    #[test]
    fn only_the_sign_in_pages_admit_turnstile() {
        let strict = csp("n0", false);
        assert!(!strict.contains("challenges.cloudflare.com"));
        assert!(strict.contains("script-src 'self';"));
        let open = csp("n0", true);
        assert!(open.contains("script-src 'self' https://challenges.cloudflare.com;"));
        assert!(open.contains("frame-src https://challenges.cloudflare.com;"));
        assert!(open.contains("frame-ancestors 'none'"));
        for p in ["/admin/signin", "/admin/setup", "/admin/invite"] {
            assert!(is_auth_page(p), "{p}");
        }
        assert!(is_auth_page("/admin/signin/"));
        for p in [
            "/admin/signin//",
            "/admin/setup///",
            "/admin/",
            "/admin/posts/1",
            "/admin/signin/x",
            "/admin/settings",
            "/admin/signinx",
        ] {
            assert!(!is_auth_page(p), "{p}");
        }
    }

    #[test]
    fn boot_json_has_the_language_but_no_text() {
        let vi = boot_json("vi", "S", "n0");
        assert_eq!(
            vi,
            "{\"lang\":\"vi\",\"langs\":[\"en\",\"vi\"],\"site\":\"S\",\"nonce\":\"n0\"}"
        );
        assert!(
            shell("vi", "S", "n0", &[], true)
                .contains("<link rel=\"modulepreload\" href=\"/admin/i18n/vi.js\">")
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
