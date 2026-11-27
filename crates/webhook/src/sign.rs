//! Standard Webhooks signing (<https://www.standardwebhooks.com>): the receiver recomputes
//! `HMAC-SHA256(secret, "{id}.{timestamp}.{body}")` and should reject timestamps more than
//! 5 minutes from its clock (replay protection is the receiver's side).

use crate::b64;

/// The bytes the HMAC covers.
#[must_use]
pub fn signed_content(msg_id: &str, timestamp_s: i64, body: &str) -> String {
    format!("{msg_id}.{timestamp_s}.{body}")
}

/// `webhook-signature` value for one HMAC result.
#[must_use]
pub fn signature_header(mac: &[u8; 32]) -> String {
    format!("v1,{}", b64::encode(mac))
}

/// Every header a delivery sends, in a fixed order.
#[must_use]
pub fn headers(msg_id: &str, timestamp_s: i64, mac: &[u8; 32]) -> Vec<(&'static str, String)> {
    vec![
        ("content-type", "application/json".to_owned()),
        ("webhook-id", msg_id.to_owned()),
        ("webhook-timestamp", timestamp_s.to_string()),
        ("webhook-signature", signature_header(mac)),
    ]
}

#[cfg(test)]
mod tests {
    use super::{headers, signature_header, signed_content};

    #[test]
    fn content_is_id_dot_timestamp_dot_body() {
        assert_eq!(
            signed_content(
                "msg_p5jXN8AQM9LWM0D4loKWxJek",
                1_614_265_330,
                r#"{"test": 2432232314}"#
            ),
            r#"msg_p5jXN8AQM9LWM0D4loKWxJek.1614265330.{"test": 2432232314}"#
        );
    }

    #[test]
    fn header_values() {
        let mac = [0xfbu8; 32];
        assert_eq!(
            signature_header(&mac),
            "v1,+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/s="
        );
        let h = headers("msg_1", 1_700_000_000, &mac);
        assert_eq!(
            h.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            [
                "content-type",
                "webhook-id",
                "webhook-timestamp",
                "webhook-signature"
            ]
        );
        assert_eq!(h[1].1, "msg_1");
        assert_eq!(h[2].1, "1700000000");
        assert_eq!(h[3].1, signature_header(&mac));
    }
}
