//! ES256 signatures arrive as DER `SEQUENCE { INTEGER r, INTEGER s }`; `WebCrypto` verifies
//! the raw 64-byte `r || s`. The conversion is strict DER (minimal lengths and integers, no
//! trailing bytes) and refuses r or s outside `1..n`, so one signature has exactly one encoding.

use plumblog_model::{Error, ErrorCode, Result};

/// Order of the P-256 group, big-endian.
const P256_ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
];

fn invalid(detail: &str) -> Error {
    Error::new(
        ErrorCode::PasskeyInvalid,
        format!("ES256 signature: {detail}"),
    )
}

/// Reads one DER INTEGER from the front of `input` as a 32-byte scalar in `1..n`; returns it and
/// the rest of the input.
fn scalar(input: &[u8]) -> Result<([u8; 32], &[u8])> {
    let (&tag, rest) = input.split_first().ok_or_else(|| invalid("truncated"))?;
    if tag != 0x02 {
        return Err(invalid("expected INTEGER"));
    }
    let (&len, rest) = rest.split_first().ok_or_else(|| invalid("truncated"))?;
    let len = usize::from(len);
    // Short-form length only: a 33-byte integer never needs the long form.
    if len == 0 || len > 33 || len > rest.len() {
        return Err(invalid("bad INTEGER length"));
    }
    let (content, rest) = rest.split_at(len);
    if content[0] & 0x80 != 0 {
        return Err(invalid("negative INTEGER"));
    }
    if len > 1 && content[0] == 0 && content[1] & 0x80 == 0 {
        return Err(invalid("non-minimal INTEGER"));
    }
    let digits = if content[0] == 0 {
        &content[1..]
    } else {
        content
    };
    if digits.len() > 32 {
        return Err(invalid("INTEGER longer than 32 bytes"));
    }
    let mut out = [0u8; 32];
    out[32 - digits.len()..].copy_from_slice(digits);
    // Big-endian arrays of equal length compare like the numbers they hold.
    if out == [0u8; 32] || out >= P256_ORDER {
        return Err(invalid("scalar out of range"));
    }
    Ok((out, rest))
}

/// DER ECDSA signature to the raw `r || s` form.
///
/// # Errors
///
/// [`ErrorCode::PasskeyInvalid`] for anything but a strict DER encoding of `r, s` in `1..n`.
pub fn ecdsa_der_to_raw(der: &[u8]) -> Result<[u8; 64]> {
    let [0x30, len, body @ ..] = der else {
        return Err(invalid("expected SEQUENCE"));
    };
    if *len >= 0x80 || usize::from(*len) != body.len() {
        return Err(invalid("bad SEQUENCE length"));
    }
    let (r, rest) = scalar(body)?;
    let (s, rest) = scalar(rest)?;
    if !rest.is_empty() {
        return Err(invalid("trailing bytes"));
    }
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&r);
    out[32..].copy_from_slice(&s);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{P256_ORDER, ecdsa_der_to_raw};
    use crate::testutil::Prng;
    use crate::webauthn::tests::raw_to_der;
    use plumblog_model::ErrorCode;

    #[test]
    fn round_trips_high_bit_short_and_typical_scalars() {
        let mut high = [0x80u8; 64];
        high[..4].fill(0xaa); // r needs a 0x00 prefix in DER
        let mut short = [0u8; 64];
        short[31] = 1; // r = 1
        short[63] = 7; // s = 7
        let mut max = [0u8; 64];
        max[..32].copy_from_slice(&P256_ORDER);
        max[31] -= 1; // r = n - 1
        max[63] = 1;
        for raw in [high, short, max, [0x11; 64]] {
            let der = raw_to_der(&raw);
            assert_eq!(ecdsa_der_to_raw(&der).unwrap(), raw, "{der:02x?}");
        }
        assert_eq!(raw_to_der(&short), vec![0x30, 6, 2, 1, 1, 2, 1, 7]);
    }

    #[test]
    fn refuses_non_strict_der() {
        let cases: &[(&str, &[u8])] = &[
            ("empty", &[]),
            ("not a sequence", &[0x31, 6, 2, 1, 1, 2, 1, 7]),
            ("long-form length", &[0x30, 0x81, 6, 2, 1, 1, 2, 1, 7]),
            ("length too long", &[0x30, 7, 2, 1, 1, 2, 1, 7]),
            ("length too short", &[0x30, 5, 2, 1, 1, 2, 1, 7]),
            ("trailing inside", &[0x30, 7, 2, 1, 1, 2, 1, 7, 0]),
            ("trailing outside", &[0x30, 6, 2, 1, 1, 2, 1, 7, 0]),
            ("one integer", &[0x30, 3, 2, 1, 1]),
            ("not integer", &[0x30, 6, 4, 1, 1, 2, 1, 7]),
            ("zero length int", &[0x30, 5, 2, 0, 2, 1, 7]),
            ("negative r", &[0x30, 6, 2, 1, 0x81, 2, 1, 7]),
            ("non-minimal r", &[0x30, 7, 2, 2, 0, 1, 2, 1, 7]),
            ("r = 0", &[0x30, 6, 2, 1, 0, 2, 1, 7]),
            ("s = 0", &[0x30, 6, 2, 1, 1, 2, 1, 0]),
            ("int len past end", &[0x30, 6, 2, 1, 1, 2, 9, 7]),
        ];
        for (name, der) in cases {
            let err = ecdsa_der_to_raw(der).unwrap_err();
            assert_eq!(err.code, ErrorCode::PasskeyInvalid, "{name}");
        }
        // 33 significant bytes, and r = n exactly.
        let mut big = vec![0x30, 38, 0x02, 33];
        big.extend([0x01; 33]);
        big.extend([0x02, 1, 7]);
        assert!(ecdsa_der_to_raw(&big).is_err());
        let mut at_order = vec![0x30, 38, 0x02, 33, 0x00];
        at_order.extend(P256_ORDER);
        at_order.extend([0x02, 1, 7]);
        assert!(ecdsa_der_to_raw(&at_order).is_err());
    }

    #[test]
    fn truncations_and_random_input_never_panic() {
        let mut rng = Prng(42);
        let mut cases = 0;
        for _ in 0..200 {
            let raw: [u8; 64] = rng.bytes(64).try_into().unwrap();
            let der = raw_to_der(&raw);
            for cut in 0..der.len() {
                assert!(ecdsa_der_to_raw(&der[..cut]).is_err());
                cases += 1;
            }
        }
        for _ in 0..5_000 {
            let len = rng.below(80);
            let mut input = rng.bytes(len);
            if len > 2 && rng.below(2) == 0 {
                input[0] = 0x30;
                input[1] = u8::try_from(len - 2).unwrap();
            }
            let _ = ecdsa_der_to_raw(&input);
            cases += 1;
        }
        assert!(cases > 15_000, "{cases}");
    }
}
