//! WCAG 2.x contrast of two opaque hex colors.

/// `#rgb` or `#rrggbb` as 0..=255 channels. Anything else (alpha, `rgb()`, names) is `None`:
/// such a color has no single contrast to check.
#[must_use]
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?]),
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..=i], 16).ok().map(|v| v * 17);
            Some([d(0)?, d(1)?, d(2)?])
        }
        _ => None,
    }
}

fn luminance([r, g, b]: [u8; 3]) -> f64 {
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// Contrast ratio, 1.0 to 21.0.
#[must_use]
pub fn ratio(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ratios() {
        let black = parse_hex("#000").unwrap();
        let white = parse_hex("#ffffff").unwrap();
        assert!((ratio(black, white) - 21.0).abs() < 1e-9);
        assert!((ratio(white, white) - 1.0).abs() < 1e-9);
        // #767676 on white is the textbook 4.54:1 grey.
        let grey = parse_hex("#767676").unwrap();
        assert!((ratio(grey, white) - 4.54).abs() < 0.01);
        assert!(ratio(parse_hex("#777").unwrap(), white) < 4.5);
    }

    #[test]
    fn only_opaque_hex_parses() {
        assert!(parse_hex("#ffffffb3").is_none());
        assert!(parse_hex("white").is_none());
        assert!(parse_hex("#12345g").is_none());
    }
}
