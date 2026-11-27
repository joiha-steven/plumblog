//! What an uploaded original is, from its first bytes: the type its magic bytes say and the
//! pixel size its header declares. Header reading only; nothing is decoded.

/// How far into a file a size is looked for (JPEG segments, AVIF boxes). Headers that need
/// more are refused rather than scanned without end.
const SCAN: usize = 256 * 1024;

/// The MIME type the magic bytes say, among the upload types.
#[must_use]
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if is_avif(b) {
        Some("image/avif")
    } else {
        None
    }
}

/// `ftyp` box first, with `avif` or `avis` as major or compatible brand.
fn is_avif(b: &[u8]) -> bool {
    if b.len() < 16 || &b[4..8] != b"ftyp" {
        return false;
    }
    let size = be32(b, 0).unwrap_or(0) as usize;
    let end = size.min(b.len());
    if size < 16 {
        return false;
    }
    // Major brand at 8, minor version at 12, compatible brands from 16.
    std::iter::once(8)
        .chain((16..end).step_by(4))
        .any(|i| matches!(b.get(i..i + 4), Some(b"avif" | b"avis")))
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 2)?;
    Some(u32::from(s[0]) << 8 | u32::from(s[1]))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 2)?;
    Some(u32::from(s[1]) << 8 | u32::from(s[0]))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[2]) << 16 | u32::from(s[1]) << 8 | u32::from(s[0]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// The pixel sizes the header declares, `(width, height)`. One for most types; AVIF lists
/// every `ispe` box in reach (primary item, grid tiles, alpha, thumbnails), as finding the
/// primary item would mean parsing `meta` in full.
#[must_use]
pub fn sizes(mime: &str, b: &[u8]) -> Vec<(u32, u32)> {
    let one = match mime {
        "image/png" if b.get(12..16) == Some(b"IHDR") => be32(b, 16).zip(be32(b, 20)),
        "image/gif" => le16(b, 6).zip(le16(b, 8)),
        "image/jpeg" => jpeg(b),
        "image/webp" => webp(b),
        "image/avif" => return ispe(b),
        _ => None,
    };
    one.into_iter().collect()
}

/// Width and height of the first start-of-frame segment.
fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= b.len().min(SCAN) {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        // Fill bytes, and markers without a length.
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        // SOF0-SOF15 except DHT (C4), JPG (C8) and DAC (CC).
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return be16(b, i + 7).zip(be16(b, i + 5));
        }
        if marker == 0xDA || len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Canvas size of a simple (`VP8 `, `VP8L`) or extended (`VP8X`) WebP.
fn webp(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
        b"VP8L" if b.get(20) == Some(&0x2F) => {
            let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8 " if b.get(23..26) == Some(&[0x9D, 0x01, 0x2A]) => {
            Some((le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF))
        }
        _ => None,
    }
}

/// Every `ispe` (image spatial extent) box in reach: `size type version+flags width height`.
fn ispe(b: &[u8]) -> Vec<(u32, u32)> {
    let end = b.len().min(SCAN);
    b[..end]
        .windows(4)
        .enumerate()
        .filter(|(i, w)| *w == b"ispe" && *i >= 4 && be32(b, i - 4) == Some(20))
        .filter_map(|(i, _)| be32(b, i + 8).zip(be32(b, i + 12)))
        .collect()
}

/// Whether the declared `width` x `height` is what the header says. JPEG and AVIF may also
/// match turned a quarter (EXIF orientation, `irot`): the declared size is what a reader sees.
#[must_use]
pub fn size_matches(mime: &str, b: &[u8], width: i64, height: i64) -> bool {
    let turnable = matches!(mime, "image/jpeg" | "image/avif");
    sizes(mime, b).into_iter().any(|(w, h)| {
        let (w, h) = (i64::from(w), i64::from(h));
        (w, h) == (width, height) || (turnable && (h, w) == (width, height))
    })
}

#[cfg(test)]
mod tests {
    use super::{size_matches, sizes, sniff};

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xFF, 0xC2, 0, 11, 8,
        ];
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([1, 1, 0x11, 0, 0xFF, 0xD9]);
        b
    }

    fn avif(sizes: &[(u32, u32)]) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 20];
        b.extend(b"ftypmif1\0\0\0\0avif");
        for (w, h) in sizes {
            b.extend([0, 0, 0, 20]);
            b.extend(b"ispe\0\0\0\0");
            b.extend(w.to_be_bytes());
            b.extend(h.to_be_bytes());
        }
        b
    }

    #[test]
    fn magic_bytes() {
        assert_eq!(sniff(&png(1, 1)), Some("image/png"));
        assert_eq!(sniff(b"GIF89a\x01\0\x01\0"), Some("image/gif"));
        assert_eq!(sniff(&jpeg(1, 1)), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8X"), Some("image/webp"));
        assert_eq!(sniff(&avif(&[])), Some("image/avif"));
        let mut heic = avif(&[]);
        heic[16..20].copy_from_slice(b"heic");
        assert_eq!(sniff(&heic), None);
        for bad in [
            &b""[..],
            b"<svg>",
            b"\x89PNG",
            b"RIFF\0\0\0\0WAVE",
            b"\0\0\0\x08ftyp",
        ] {
            assert_eq!(sniff(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn declared_sizes() {
        assert!(size_matches("image/png", &png(640, 480), 640, 480));
        assert!(!size_matches("image/png", &png(640, 480), 480, 640));
        assert!(size_matches(
            "image/gif",
            b"GIF89a\x80\x02\xe0\x01",
            640,
            480
        ));
        assert!(size_matches("image/jpeg", &jpeg(640, 480), 640, 480));
        // EXIF may turn a JPEG; the declared size is the one a reader sees.
        assert!(size_matches("image/jpeg", &jpeg(640, 480), 480, 640));
        assert!(!size_matches("image/jpeg", &jpeg(640, 480), 640, 481));
        // A segment before the frame header is skipped by its length.
        let mut app = vec![0xFF, 0xD8, 0xFF, 0xE1, 0, 6, 1, 2, 3, 4];
        app.extend(&jpeg(3, 2)[2..]);
        assert_eq!(sizes("image/jpeg", &app), vec![(3, 2)]);
        let mut x = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        x.extend([0x7F, 0x02, 0, 0xDF, 0x01, 0]);
        assert_eq!(sizes("image/webp", &x), vec![(640, 480)]);
        let mut l = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        l.extend((0x27Fu32 | (0x1DF << 14)).to_le_bytes());
        assert_eq!(sizes("image/webp", &l), vec![(640, 480)]);
        let mut v = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0\0\0\0\x9d\x01\x2a".to_vec();
        v.extend([0x80, 0x02, 0xE0, 0x01]);
        assert_eq!(sizes("image/webp", &v), vec![(640, 480)]);
        assert!(size_matches(
            "image/avif",
            &avif(&[(160, 120), (640, 480)]),
            640,
            480
        ));
        assert!(!size_matches("image/avif", &avif(&[]), 640, 480));
        // Truncated headers never panic and never match.
        for mime in [
            "image/png",
            "image/gif",
            "image/jpeg",
            "image/webp",
            "image/avif",
        ] {
            for n in 0..30 {
                assert!(!size_matches(mime, &png(1, 1)[..n.min(25)], 1, 2));
                let _ = sizes(mime, &x[..n.min(x.len())]);
                let _ = sizes(mime, &jpeg(1, 1)[..n.min(22)]);
            }
        }
    }
}
