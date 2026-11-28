//! Files under `/_theme/` and `/plugin/` are public. Refuse names and contents that look like
//! credentials before a user publishes one by accident.

/// Why a file name is refused, or `None`.
#[must_use]
pub fn bad_name(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name == ".env" || name.starts_with(".env.") {
        return Some(".env");
    }
    for ext in [".pem", ".key", ".p12", ".pfx"] {
        if name.ends_with(ext) {
            return Some(ext);
        }
    }
    if name.starts_with("id_rsa") || name.starts_with("id_ed25519") {
        return Some("id_rsa");
    }
    ["secret", "token", "credential"]
        .into_iter()
        .find(|w| name.contains(w))
}

/// `(line, kind)` of the first match per kind in `text`: kinds are `private key`,
/// `AWS access key`, `GitHub token` and `API token` (a 40-character mixed-case run, the
/// shape of a Cloudflare API token).
#[must_use]
pub fn in_text(text: &str) -> Vec<(usize, &'static str)> {
    let mut found: Vec<(usize, &'static str)> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let mut add = |kind: &'static str| {
            if !found.iter().any(|(_, k)| *k == kind) {
                found.push((n, kind));
            }
        };
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY-----") {
            add("private key");
        }
        if runs(
            line,
            16,
            |c| c.is_ascii_uppercase() || c.is_ascii_digit(),
            "AKIA",
        ) {
            add("AWS access key");
        }
        if runs(line, 36, |c| c.is_ascii_alphanumeric(), "ghp_")
            || runs(
                line,
                22,
                |c| c.is_ascii_alphanumeric() || c == '_',
                "github_pat_",
            )
        {
            add("GitHub token");
        }
        if cloudflare_like(line) {
            add("API token");
        }
    }
    found
}

/// `prefix` followed by at least `min` characters of `class`.
fn runs(line: &str, min: usize, class: impl Fn(char) -> bool, prefix: &str) -> bool {
    line.match_indices(prefix).any(|(i, _)| {
        let before_ok = line[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        before_ok
            && line[i + prefix.len()..]
                .chars()
                .take_while(|c| class(*c))
                .count()
                >= min
    })
}

/// A maximal run of exactly 40 of `[A-Za-z0-9_-]` holding upper case, lower case and a digit.
/// Hashes (hex) and words do not have all three.
fn cloudflare_like(line: &str) -> bool {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .any(|w| {
            w.len() == 40
                && w.bytes().any(|b| b.is_ascii_uppercase())
                && w.bytes().any(|b| b.is_ascii_lowercase())
                && w.bytes().any(|b| b.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for bad in [
            ".env",
            "a/.env.local",
            "x.pem",
            "k.KEY",
            "id_rsa.pub",
            "my-secret.css",
            "assets/Token.js",
        ] {
            assert!(bad_name(bad).is_some(), "{bad}");
        }
        for ok in [
            "style.css",
            "assets/app.js",
            "templates/post.html",
            "envelope.png",
        ] {
            assert!(bad_name(ok).is_none(), "{ok}");
        }
    }

    #[test]
    fn contents() {
        let kinds = |s: &str| in_text(s).into_iter().map(|(_, k)| k).collect::<Vec<_>>();
        assert_eq!(
            kinds("/* -----BEGIN RSA PRIVATE KEY----- */"),
            ["private key"]
        );
        assert_eq!(kinds("k=AKIAIOSFODNN7EXAMPLE"), ["AWS access key"]);
        assert_eq!(
            kinds("ghp_0123456789abcdefghijklmnopqrstuvwxyz"),
            ["GitHub token"]
        );
        assert_eq!(
            kinds("github_pat_11ABCDEFG0abcdefghijkl_xyz"),
            ["GitHub token"]
        );
        assert_eq!(
            kinds("/* Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0UvWxYz1234 */"),
            ["API token"]
        );
        // A sha1 (hex), a long class name and a short word are not tokens.
        assert!(kinds("da39a3ee5e6b4b0d3255bfef95601890afd80709").is_empty());
        assert!(kinds(".some-very-long-class-name-for-the-layout-x").is_empty());
        assert!(kinds("a { color: red }").is_empty());
        assert_eq!(in_text("x\n\n/* AKIAIOSFODNN7EXAMPLE */")[0].0, 3);
    }
}
