//! A post's cover as the theme gets it: the stored size best for a social card (the head's
//! `og:image`) that the cover figure shows too, with the same-type sizes as `srcset`.

use plumblog_auth::json::{self, Value};
use plumblog_render::Image;
use plumblog_store::ReaderCover;

use super::warn;

/// Widest size a social card takes; larger ones are cropped or refused by the networks.
const CARD_MAX_WIDTH: i64 = 1200;

struct Size {
    name: String,
    mime: String,
    width: i64,
    height: i64,
}

/// File name as the media route knows it (last key segment), safe in a URL path.
fn url_name(key: &str) -> String {
    use std::fmt::Write as _;
    let name = key.rsplit('/').next().unwrap_or_default();
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The stored sizes; a row that is not what the admin encoder writes has none, and is logged.
fn sizes(c: &ReaderCover) -> Vec<Size> {
    // The same wrapping as the media routes: the strict reader takes an object at the top.
    let wrapped = format!("{{\"v\":{}}}", c.variants_json);
    let items = match json::parse_document(wrapped.as_bytes(), usize::MAX) {
        Ok(mut doc) if doc.len() == 1 => match doc.remove(0).1 {
            Value::Arr(items) => items,
            // `{}` is what older rows and fixtures hold.
            Value::Obj(o) if o.is_empty() => Vec::new(),
            _ => unreadable(c),
        },
        _ => unreadable(c),
    };
    items.into_iter().filter_map(size).collect()
}

fn unreadable(c: &ReaderCover) -> Vec<Value> {
    warn(&format!(
        "media.variants_unreadable: media {} variants_json is not an array; the cover uses the original",
        c.id.0
    ));
    Vec::new()
}

fn size(v: Value) -> Option<Size> {
    let Value::Obj(fields) = v else { return None };
    let (mut key, mut mime, mut width, mut height) = (None, String::new(), 0, 0);
    for (k, v) in fields {
        match (k.as_str(), v) {
            ("key", Value::Str(t)) => key = Some(t),
            ("mime", Value::Str(t)) => mime = t,
            ("width", Value::Int(n)) => width = n,
            ("height", Value::Int(n)) => height = n,
            _ => {}
        }
    }
    (width > 0 && height > 0).then_some(())?;
    Some(Size {
        name: url_name(&key?),
        mime,
        width,
        height,
    })
}

fn widest(pool: Vec<&Size>) -> Option<&Size> {
    pool.into_iter().max_by_key(|s| s.width)
}

/// Largest size up to [`CARD_MAX_WIDTH`], WebP and the like before AVIF (social networks
/// read AVIF rarely); the original when there is none.
pub(super) fn build(c: &ReaderCover) -> Image {
    let all = sizes(c);
    let fits = |s: &&Size| s.width <= CARD_MAX_WIDTH;
    let chosen = widest(
        all.iter()
            .filter(fits)
            .filter(|s| s.mime != "image/avif")
            .collect(),
    )
    .or_else(|| widest(all.iter().filter(fits).collect()));
    let id = c.id.0;
    let Some(s) = chosen else {
        return Image {
            src: format!("/media/{id}/{}", url_name(&c.r2_key)),
            srcset: None,
            width: u32::try_from(c.width).unwrap_or(0),
            height: u32::try_from(c.height).unwrap_or(0),
            alt: c.alt.clone(),
        };
    };
    let mut same: Vec<&Size> = all.iter().filter(|o| o.mime == s.mime).collect();
    same.sort_by_key(|o| o.width);
    let srcset = (same.len() > 1).then(|| {
        same.iter()
            .map(|o| format!("/media/{id}/{} {}w", o.name, o.width))
            .collect::<Vec<_>>()
            .join(", ")
    });
    Image {
        src: format!("/media/{id}/{}", s.name),
        srcset,
        width: u32::try_from(s.width).unwrap_or(0),
        height: u32::try_from(s.height).unwrap_or(0),
        alt: c.alt.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plumblog_model::MediaId;

    fn cover(variants: &str) -> ReaderCover {
        ReaderCover {
            id: MediaId(7),
            r2_key: "media/7/my photo.png".into(),
            width: 3000,
            height: 1500,
            alt: "alt".into(),
            variants_json: variants.into(),
        }
    }

    fn v(name: &str, w: i64, mime: &str) -> String {
        format!(
            r#"{{"key":"media/7/{name}","width":{w},"height":{},"bytes":1,"mime":"{mime}"}}"#,
            w / 2
        )
    }

    #[test]
    fn picks_the_largest_non_avif_size_within_the_card_width() {
        let json = format!(
            "[{},{},{},{},{}]",
            v("a-480.webp", 480, "image/webp"),
            v("a-960.webp", 960, "image/webp"),
            v("a-1600.webp", 1600, "image/webp"),
            v("a-960.avif", 960, "image/avif"),
            v("a-1200.avif", 1200, "image/avif")
        );
        let img = build(&cover(&json));
        assert_eq!(img.src, "/media/7/a-960.webp");
        assert_eq!((img.width, img.height), (960, 480));
        assert_eq!(
            img.srcset.as_deref(),
            Some("/media/7/a-480.webp 480w, /media/7/a-960.webp 960w, /media/7/a-1600.webp 1600w")
        );
    }

    #[test]
    fn falls_back_to_avif_then_to_the_original() {
        let avif = format!("[{}]", v("a-960.avif", 960, "image/avif"));
        let img = build(&cover(&avif));
        assert_eq!((img.src.as_str(), img.width), ("/media/7/a-960.avif", 960));
        assert_eq!(img.srcset, None);
        // Only sizes wider than a card: the original, whose name is escaped for the URL.
        let big = format!("[{}]", v("a-1600.webp", 1600, "image/webp"));
        for json in [big.as_str(), "[]", "{}", "not json"] {
            let img = build(&cover(json));
            assert_eq!(img.src, "/media/7/my%20photo.png", "{json}");
            assert_eq!((img.width, img.height), (3000, 1500));
        }
    }
}
