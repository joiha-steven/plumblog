//! Which URLs a webhook may call. Checked when a webhook is saved and again before every
//! send, on the URL text alone (no DNS: names are resolved by Cloudflare's fetch, which does
//! not reach private networks; this guard stops what can be seen in the text).
//!
//! The parser is deliberately narrower than the WHATWG URL parser `fetch` uses, so the two
//! cannot disagree about the host: anything outside printable ASCII, a backslash, a percent
//! sign or userinfo in the authority, and any host that WHATWG would read as an IPv4 number
//! but that is not a canonical dotted quad (`2130706433`, `0x7f.1`, `0177.0.0.1`, `127.1`)
//! is refused rather than interpreted.

use std::net::{Ipv4Addr, Ipv6Addr};

use plumblog_model::stable_codes;

stable_codes! {
    /// Why a URL was refused; logged and asserted by tests.
    pub enum Blocked {
        Malformed => "malformed",
        NotHttps => "not_https",
        Userinfo => "userinfo",
        BadPort => "bad_port",
        BadHost => "bad_host",
        NonCanonicalIp => "noncanonical_ip",
        IpNotPublic => "ip_not_public",
        ReservedName => "reserved_name",
        SingleLabel => "single_label",
        NonAsciiHost => "non_ascii_host",
    }
}

/// Which rules apply. Code outside tests only ever builds [`Policy::Production`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Production,
    /// Also allows `http://127.0.0.1:<port>/...` for the local test receiver. Exists only
    /// with the `test-receiver` feature, which deploys never enable.
    #[cfg(any(test, feature = "test-receiver"))]
    AllowLoopbackHttpForTests,
}

/// An allowed URL's host (lowercase; IPv6 in brackets) and explicit port, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: Option<u16>,
}

const MAX_URL: usize = 2048;

/// Names that only mean something inside a network (RFC 6761, 6762, 8375) or to a cloud
/// metadata service (`metadata.google.internal`), matched as the name or any subdomain.
const RESERVED_SUFFIXES: &[&str] = &["localhost", "local", "internal", "home.arpa"];

/// Checks `url` under `policy`.
///
/// # Errors
/// The first rule the URL breaks.
pub fn check(url: &str, policy: Policy) -> Result<Target, Blocked> {
    // Before the ASCII-only rule, so a Unicode host gets its own reason: the writer must send
    // the punycode (`xn--`) form, which no part of the Worker converts for them.
    if let Some((_, rest)) = url.split_once("://") {
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if url.len() <= MAX_URL && !rest[..end].is_ascii() {
            return Err(Blocked::NonAsciiHost);
        }
    }
    if url.len() > MAX_URL
        || !url
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'\\')
    {
        return Err(Blocked::Malformed);
    }
    let (scheme, rest) = url.split_once("://").ok_or(Blocked::Malformed)?;
    let scheme = scheme.to_ascii_lowercase();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(Blocked::Userinfo);
    }
    if authority.contains('%') {
        return Err(Blocked::BadHost);
    }
    let (host, port) = split_port(authority)?;
    if host.is_empty() {
        return Err(Blocked::Malformed);
    }
    let host = host.to_ascii_lowercase();

    #[cfg(any(test, feature = "test-receiver"))]
    if policy == Policy::AllowLoopbackHttpForTests
        && scheme == "http"
        && host == "127.0.0.1"
        && port.is_some()
    {
        return Ok(Target { host, port });
    }
    let _ = policy;
    if scheme != "https" {
        return Err(Blocked::NotHttps);
    }

    if let Some(inner) = host.strip_prefix('[') {
        let ip: Ipv6Addr = inner
            .strip_suffix(']')
            .and_then(|s| s.parse().ok())
            .ok_or(Blocked::Malformed)?;
        if !public_v6(ip) {
            return Err(Blocked::IpNotPublic);
        }
        return Ok(Target { host, port });
    }
    check_name(&host)?;
    Ok(Target { host, port })
}

/// Any port is allowed: the host rules are what keep requests off internal networks, and
/// receivers on e.g. 8443 are legitimate. An empty port (`host:`) is refused as ambiguous.
fn split_port(authority: &str) -> Result<(&str, Option<u16>), Blocked> {
    let (host, port) = if authority.starts_with('[') {
        let close = authority.find(']').ok_or(Blocked::Malformed)?;
        let (h, after) = authority.split_at(close + 1);
        match after {
            "" => (h, None),
            p => (h, Some(p.strip_prefix(':').ok_or(Blocked::Malformed)?)),
        }
    } else {
        match authority.split_once(':') {
            None => (authority, None),
            Some((h, p)) => (h, Some(p)),
        }
    };
    let Some(p) = port else {
        return Ok((host, None));
    };
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Blocked::BadPort);
    }
    match p.parse::<u16>() {
        Ok(n) if n > 0 => Ok((host, Some(n))),
        _ => Err(Blocked::BadPort),
    }
}

fn check_name(host: &str) -> Result<(), Blocked> {
    if !host
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(Blocked::BadHost);
    }
    // One trailing dot is the DNS root; `localhost.` is still localhost.
    let name = host.strip_suffix('.').unwrap_or(host);
    let labels: Vec<&str> = name.split('.').collect();
    if name.is_empty() || name.len() > 253 || labels.iter().any(|l| l.is_empty() || l.len() > 63) {
        return Err(Blocked::BadHost);
    }
    // WHATWG reads a host whose last label is a number (decimal, or 0x hex) as IPv4, in
    // forms we refuse to second-guess; only the canonical dotted quad is accepted.
    let last = labels[labels.len() - 1];
    let hex = last
        .strip_prefix("0x")
        .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
    if hex || last.bytes().all(|b| b.is_ascii_digit()) {
        let ip = canonical_v4(host).ok_or(Blocked::NonCanonicalIp)?;
        return if public_v4(ip) {
            Ok(())
        } else {
            Err(Blocked::IpNotPublic)
        };
    }
    if labels
        .iter()
        .any(|l| l.starts_with('-') || l.ends_with('-'))
    {
        return Err(Blocked::BadHost);
    }
    let reserved = RESERVED_SUFFIXES
        .iter()
        .any(|s| name == *s || name.strip_suffix(s).is_some_and(|head| head.ends_with('.')));
    if reserved {
        return Err(Blocked::ReservedName);
    }
    if labels.len() < 2 {
        // A bare name resolves through local search domains, never to a public receiver.
        return Err(Blocked::SingleLabel);
    }
    Ok(())
}

/// `a.b.c.d`, each 0-255 in plain decimal without leading zeros; nothing else.
fn canonical_v4(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (o, p) in octets.iter_mut().zip(&parts) {
        let ok = !p.is_empty()
            && p.len() <= 3
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p.len() == 1 || !p.starts_with('0'));
        if !ok {
            return None;
        }
        *o = p.parse().ok()?;
    }
    Some(Ipv4Addr::from(octets))
}

fn in_v4(ip: Ipv4Addr, net: [u8; 4], bits: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(net)) & mask
}

/// Not loopback, private, shared (CGNAT), link-local (incl. 169.254.169.254 metadata),
/// "this network", documentation, benchmarking, multicast, reserved or broadcast.
fn public_v4(ip: Ipv4Addr) -> bool {
    const BLOCKED: &[([u8; 4], u32)] = &[
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 88, 99, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ];
    !BLOCKED.iter().any(|(net, bits)| in_v4(ip, *net, *bits))
}

/// Only global unicast (`2000::/3`), minus documentation (`2001:db8::/32`), Teredo
/// (`2001::/32`) and 6to4 (`2002::/16`), which tunnel to IPv4 addresses. Everything else is
/// refused: `::`, `::1`, IPv4-mapped and -compatible forms, NAT64, ULA (`fc00::/7`, incl. the
/// AWS metadata address `fd00:ec2::254`), link-local and multicast.
fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let global = s[0] & 0xe000 == 0x2000;
    let tunnel_or_doc = (s[0] == 0x2001 && (s[1] == 0 || s[1] == 0x0db8)) || s[0] == 0x2002;
    global && !tunnel_or_doc
}

#[cfg(test)]
mod tests {
    use super::{Blocked, Policy, Target, check};

    #[test]
    fn accepts_public_https() {
        for (url, host, port) in [
            ("https://example.com", "example.com", None),
            (
                "https://hooks.Example.COM/in?x=1#f",
                "hooks.example.com",
                None,
            ),
            ("HTTPS://example.com/", "example.com", None),
            ("https://example.com:8443/hook", "example.com", Some(8443)),
            ("https://example.com:443/", "example.com", Some(443)),
            ("https://example.com./", "example.com.", None),
            (
                "https://xn--bcher-kva.example/",
                "xn--bcher-kva.example",
                None,
            ),
            ("https://a-b.c0.example/", "a-b.c0.example", None),
            ("https://93.184.216.34/", "93.184.216.34", None),
            ("https://8.8.8.8:8080/", "8.8.8.8", Some(8080)),
            (
                "https://[2606:4700:4700::1111]/",
                "[2606:4700:4700::1111]",
                None,
            ),
            ("https://example.com/@x/%41", "example.com", None),
            ("https://notlocalhost.com/", "notlocalhost.com", None),
            ("https://local.example.com/", "local.example.com", None),
        ] {
            assert_eq!(
                check(url, Policy::Production),
                Ok(Target {
                    host: host.into(),
                    port
                }),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_with_reason() {
        use Blocked::{
            BadHost, BadPort, IpNotPublic, Malformed, NonAsciiHost, NonCanonicalIp, NotHttps,
            ReservedName, SingleLabel, Userinfo,
        };
        for (url, why) in [
            // Scheme.
            ("http://example.com/", NotHttps),
            ("ftp://example.com/", NotHttps),
            ("javascript://example.com/", NotHttps),
            ("example.com", Malformed),
            ("https:example.com", Malformed),
            ("https:///path", Malformed),
            ("", Malformed),
            // Userinfo, odd characters, parser differentials.
            ("https://user:pw@example.com/", Userinfo),
            ("https://user@example.com/", Userinfo),
            ("https://example.com@127.0.0.1/", Userinfo),
            ("https://example.com\\@127.0.0.1/", Malformed),
            ("https://exa mple.com/", Malformed),
            ("https://exa\tmple.com/", Malformed),
            ("https://127.0.0.\n1/", Malformed),
            ("https://b\u{fc}cher.example/", NonAsciiHost),
            ("https://\u{4f8b}.example:8443/x", NonAsciiHost),
            ("https://%31%32%37.0.0.1/", BadHost),
            ("https://exa_mple.com/", BadHost),
            ("https://example..com/", BadHost),
            ("https://.example.com/", BadHost),
            ("https://example.com../", BadHost),
            ("https://-example.com/", BadHost),
            // Ports.
            ("https://example.com:/", BadPort),
            ("https://example.com:0/", BadPort),
            ("https://example.com:65536/", BadPort),
            ("https://example.com:+443/", BadPort),
            ("https://example.com:443:1/", BadPort),
            // Non-canonical IPv4 (WHATWG would read these as 127.0.0.1).
            ("http://2130706433", NotHttps),
            ("https://2130706433/", NonCanonicalIp),
            ("https://0x7f.1/", NonCanonicalIp),
            ("https://0x7f000001/", NonCanonicalIp),
            ("https://0177.0.0.1/", NonCanonicalIp),
            ("https://127.1/", NonCanonicalIp),
            ("https://127.0.1/", NonCanonicalIp),
            ("https://127.0.0.1./", NonCanonicalIp),
            ("https://1.2.3.4.5/", NonCanonicalIp),
            ("https://08.8.8.8/", NonCanonicalIp),
            ("https://8.8.8.256/", NonCanonicalIp),
            ("https://foo.0x10/", NonCanonicalIp),
            ("https://foo.123/", NonCanonicalIp),
            // Non-public IPv4.
            ("https://127.0.0.1/", IpNotPublic),
            ("https://127.255.255.254:8443/", IpNotPublic),
            ("https://0.0.0.0/", IpNotPublic),
            ("https://10.1.2.3/", IpNotPublic),
            ("https://172.16.0.1/", IpNotPublic),
            ("https://172.31.255.255/", IpNotPublic),
            ("https://192.168.1.1/", IpNotPublic),
            ("https://169.254.169.254/", IpNotPublic),
            ("https://169.254.1.1/", IpNotPublic),
            ("https://100.64.0.1/", IpNotPublic),
            ("https://100.127.255.255/", IpNotPublic),
            ("https://224.0.0.1/", IpNotPublic),
            ("https://239.255.255.250/", IpNotPublic),
            ("https://255.255.255.255/", IpNotPublic),
            ("https://240.0.0.1/", IpNotPublic),
            ("https://192.0.2.1/", IpNotPublic),
            ("https://198.18.0.1/", IpNotPublic),
            // IPv6.
            ("https://[::1]/", IpNotPublic),
            ("https://[::]/", IpNotPublic),
            ("https://[::ffff:127.0.0.1]/", IpNotPublic),
            ("https://[::ffff:7f00:1]/", IpNotPublic),
            ("https://[::ffff:8.8.8.8]/", IpNotPublic),
            ("https://[::127.0.0.1]/", IpNotPublic),
            ("https://[64:ff9b::7f00:1]/", IpNotPublic),
            ("https://[fe80::1]/", IpNotPublic),
            ("https://[fc00::1]/", IpNotPublic),
            ("https://[fd00:ec2::254]/", IpNotPublic),
            ("https://[ff02::1]/", IpNotPublic),
            ("https://[2001:db8::1]/", IpNotPublic),
            ("https://[2002:7f00:1::1]/", IpNotPublic),
            ("https://[2001:0:4136:e378::1]/", IpNotPublic),
            ("https://[fe80::1%25en0]/", BadHost),
            ("https://[::1/", Malformed),
            ("https://[nope]/", Malformed),
            ("https://[2606:4700::1111]x/", Malformed),
            // Reserved and bare names.
            ("https://localhost/", ReservedName),
            ("https://LOCALHOST/", ReservedName),
            ("https://localhost./", ReservedName),
            ("https://api.localhost/", ReservedName),
            ("https://printer.local/", ReservedName),
            ("https://printer.local./", ReservedName),
            ("https://db.internal/", ReservedName),
            ("https://metadata.google.internal/", ReservedName),
            ("https://metadata.google.internal./", ReservedName),
            ("https://nas.home.arpa/", ReservedName),
            ("https://home.arpa/", ReservedName),
            ("https://intranet/", SingleLabel),
            ("https://intranet./", SingleLabel),
        ] {
            assert_eq!(check(url, Policy::Production), Err(why), "{url}");
        }
    }

    #[test]
    fn test_policy_only_opens_loopback_http_with_a_port() {
        let t = Policy::AllowLoopbackHttpForTests;
        assert_eq!(
            check("http://127.0.0.1:8788/hook", t),
            Ok(Target {
                host: "127.0.0.1".into(),
                port: Some(8788)
            })
        );
        assert_eq!(check("http://127.0.0.1/", t), Err(Blocked::NotHttps));
        assert_eq!(check("http://localhost:8788/", t), Err(Blocked::NotHttps));
        assert_eq!(check("http://10.0.0.1:80/", t), Err(Blocked::NotHttps));
        assert_eq!(
            check("https://127.0.0.1:8788/", t),
            Err(Blocked::IpNotPublic)
        );
        assert_eq!(
            check("http://127.0.0.1:8788/", Policy::Production),
            Err(Blocked::NotHttps)
        );
    }

    #[test]
    fn reason_codes_are_pinned() {
        let codes: Vec<&str> = Blocked::ALL.iter().map(|b| b.as_str()).collect();
        assert_eq!(
            codes,
            [
                "malformed",
                "not_https",
                "userinfo",
                "bad_port",
                "bad_host",
                "noncanonical_ip",
                "ip_not_public",
                "reserved_name",
                "single_label",
                "non_ascii_host",
            ]
        );
    }
}
