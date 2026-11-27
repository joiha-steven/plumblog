//! Strict, in-memory readers for release archives: gzip (RFC 1952), tar (POSIX ustar, with
//! pax `x` and GNU `L` long names) and zip (one stored or deflated member). Every archive is
//! already checked against a signed or baked SHA-256 before it gets here; these readers still
//! refuse anything that could write outside the target: `..`, absolute or drive paths,
//! symlinks, hard links, devices and FIFOs. Nothing is written to disk in this module.

use super::reason::Reason;
use std::collections::BTreeMap;

/// Why an archive cannot be used: the entry and the [`Reason`] (translated when shown).
#[derive(Debug, PartialEq, Eq)]
pub struct Bad {
    pub path: String,
    pub reason: Reason,
}

fn bad(path: &str, reason: Reason) -> Bad {
    Bad {
        path: path.chars().filter(|c| !c.is_control()).collect(),
        reason,
    }
}

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const CRC: [u32; 256] = crc_table();

/// CRC-32 (IEEE), as gzip and zip store it.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in data {
        c = CRC[((c ^ u32::from(*b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

fn inflate(raw: &[u8], limit: usize, what: &str) -> Result<Vec<u8>, Bad> {
    miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit)
        .map_err(|e| bad(what, Reason::Inflate(format!("{:?}", e.status))))
}

/// One gzip member; `limit` caps the decompressed size.
///
/// # Errors
/// Not gzip, a bad header, corrupt data, a CRC or size mismatch, or over `limit`.
pub fn gunzip(data: &[u8], limit: usize) -> Result<Vec<u8>, Bad> {
    const W: &str = "gzip";
    if data.len() < 18 || data[..3] != [0x1f, 0x8b, 8] {
        return Err(bad(W, Reason::NotGzip));
    }
    let flags = data[3];
    if flags & 0xe0 != 0 {
        return Err(bad(W, Reason::GzipFlags));
    }
    let mut pos = 10;
    let need = |pos: usize, n: usize| {
        if pos + n > data.len() - 8 {
            Err(bad(W, Reason::GzipHeaderTruncated))
        } else {
            Ok(())
        }
    };
    if flags & 4 != 0 {
        need(pos, 2)?;
        let xlen = usize::from(u16::from_le_bytes([data[pos], data[pos + 1]]));
        need(pos, 2 + xlen)?;
        pos += 2 + xlen;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let end = data[pos..data.len() - 8]
                .iter()
                .position(|b| *b == 0)
                .ok_or_else(|| bad(W, Reason::GzipHeaderTruncated))?;
            pos += end + 1;
        }
    }
    if flags & 2 != 0 {
        need(pos, 2)?;
        pos += 2;
    }
    let out = inflate(&data[pos..data.len() - 8], limit, W)?;
    let t = &data[data.len() - 8..];
    let crc = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
    let size = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
    #[allow(clippy::cast_possible_truncation)]
    if crc32(&out) != crc || out.len() as u32 != size {
        return Err(bad(W, Reason::GzipChecksum));
    }
    Ok(out)
}

/// A relative path made only of normal components, `./` prefixes and a trailing `/`
/// removed. `None` for anything that could leave the target directory.
fn clean_path(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.contains(['\\', ':', '\0'])
        || raw.chars().any(char::is_control)
    {
        return None;
    }
    let mut parts: Vec<&str> = raw.split('/').collect();
    while parts.first() == Some(&".") {
        parts.remove(0);
    }
    if parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.is_empty() {
        return Some(String::new());
    }
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return None;
    }
    Some(parts.join("/"))
}

/// Regular files of an archive by path, in path order.
pub type Files = BTreeMap<String, Vec<u8>>;

fn octal(field: &[u8], path: &str) -> Result<u64, Bad> {
    let s: Vec<u8> = field
        .iter()
        .copied()
        .take_while(|b| *b != 0)
        .filter(|b| *b != b' ')
        .collect();
    if s.is_empty() {
        return Ok(0);
    }
    let s = std::str::from_utf8(&s).map_err(|_| bad(path, Reason::BadNumber))?;
    u64::from_str_radix(s, 8).map_err(|_| bad(path, Reason::BadNumber))
}

fn cstr(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    &field[..end]
}

fn utf8(b: &[u8]) -> Result<String, Bad> {
    String::from_utf8(b.to_vec()).map_err(|_| bad("?", Reason::PathNotUtf8))
}

/// pax extended header records `"<len> <key>=<value>\n"`; returns `path` and `size`.
fn pax(data: &[u8]) -> Result<(Option<String>, Option<u64>), Bad> {
    let (mut path, mut size) = (None, None);
    let mut rest = data;
    while !rest.is_empty() {
        let sp = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or_else(|| bad("pax", Reason::PaxRecord))?;
        let len: usize = std::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| *n > sp + 1 && *n <= rest.len())
            .ok_or_else(|| bad("pax", Reason::PaxLength))?;
        let rec = &rest[sp + 1..len];
        let rec = rec
            .strip_suffix(b"\n")
            .ok_or_else(|| bad("pax", Reason::PaxRecord))?;
        let eq = rec
            .iter()
            .position(|b| *b == b'=')
            .ok_or_else(|| bad("pax", Reason::PaxRecord))?;
        let (k, v) = (&rec[..eq], &rec[eq + 1..]);
        match k {
            b"path" => path = Some(utf8(v)?),
            b"size" => {
                size = Some(
                    std::str::from_utf8(v)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| bad("pax", Reason::PaxSize))?,
                );
            }
            b"linkpath" => return Err(bad("pax", Reason::PaxLink)),
            _ => {}
        }
        rest = &rest[len..];
    }
    Ok((path, size))
}

/// The path in a ustar header (`prefix/name`) or a GNU/v7 one (`name`).
fn header_path(h: &[u8]) -> Result<String, Bad> {
    let name = cstr(&h[..100]);
    let prefix = cstr(&h[345..500]);
    if &h[257..263] == b"ustar\0" && !prefix.is_empty() {
        Ok(format!("{}/{}", utf8(prefix)?, utf8(name)?))
    } else {
        utf8(name)
    }
}

/// Regular file (false) or directory (true); every other entry type is refused.
fn entry_is_dir(kind: u8, path: &str) -> Result<bool, Bad> {
    match kind {
        b'0' | 0 | b'7' => Ok(false),
        b'5' => Ok(true),
        b'1' => Err(bad(path, Reason::HardLink)),
        b'2' => Err(bad(path, Reason::Symlink)),
        b'3' | b'4' => Err(bad(path, Reason::DeviceFile)),
        b'6' => Err(bad(path, Reason::Fifo)),
        _ => Err(bad(path, Reason::EntryType(char::from(kind)))),
    }
}

/// Reads a tar archive: regular files only, each path checked by `allowed` (given the clean
/// path and whether it is a directory). Directories are checked and dropped.
///
/// # Errors
/// [`Bad`] with the entry and reason; the whole archive is refused.
pub fn untar(data: &[u8], allowed: &dyn Fn(&str, bool) -> bool) -> Result<Files, Bad> {
    let mut files = Files::new();
    let mut dirs = std::collections::BTreeSet::new();
    let mut pos = 0;
    let (mut long_path, mut pax_size): (Option<String>, Option<u64>) = (None, None);
    loop {
        let Some(h) = data.get(pos..pos + 512) else {
            return Err(bad("tar", Reason::NoEndMarker));
        };
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        let name_raw = cstr(&h[..100]);
        let shown = String::from_utf8_lossy(name_raw).into_owned();
        if octal(&h[148..156], &shown)? != sum {
            return Err(bad(&shown, Reason::HeaderChecksum));
        }
        if h[124] & 0x80 != 0 {
            return Err(bad(&shown, Reason::BinarySize));
        }
        let size = pax_size
            .take()
            .map_or_else(|| octal(&h[124..136], &shown), Ok)?;
        let start = pos + 512;
        let size_us = usize::try_from(size).map_err(|_| bad(&shown, Reason::TooLarge))?;
        let body = data
            .get(start..start.saturating_add(size_us))
            .ok_or_else(|| bad(&shown, Reason::Truncated))?;
        pos = start + size_us.div_ceil(512) * 512;
        let kind = h[156];
        match kind {
            b'x' => {
                let (p, s) = pax(body)?;
                if p.is_some() {
                    long_path = p;
                }
                pax_size = s;
                continue;
            }
            b'g' => continue,
            b'L' => {
                long_path = Some(utf8(cstr(body))?);
                continue;
            }
            _ => {}
        }
        let raw = match long_path.take() {
            Some(p) => p,
            None => header_path(h)?,
        };
        let is_dir = entry_is_dir(kind, &raw)?;
        let path = clean_path(&raw).ok_or_else(|| bad(&raw, Reason::UnsafePath))?;
        if path.is_empty() {
            if is_dir {
                continue;
            }
            return Err(bad(&raw, Reason::UnsafePath));
        }
        if !allowed(&path, is_dir) {
            return Err(bad(&path, Reason::NotAllowed));
        }
        if is_dir {
            if files.contains_key(&path) {
                return Err(bad(&path, Reason::FileAndDirectory));
            }
            dirs.insert(path);
            continue;
        }
        if dirs.contains(&path) || files.contains_key(&path) {
            return Err(bad(&path, Reason::Duplicate));
        }
        files.insert(path, body.to_vec());
    }
    // A file whose parent is another file cannot be written.
    for p in files.keys() {
        let mut parent = p.as_str();
        while let Some((up, _)) = parent.rsplit_once('/') {
            if files.contains_key(up) {
                return Err(bad(p, Reason::InsideFile));
            }
            parent = up;
        }
    }
    Ok(files)
}

fn le16(b: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// One member of a zip archive, by exact name at the top level; `None` if absent.
///
/// # Errors
/// [`Bad`] for a malformed archive, zip64, encryption, an unknown method, a CRC mismatch.
pub fn unzip_one(data: &[u8], name: &str, limit: usize) -> Result<Option<Vec<u8>>, Bad> {
    const W: &str = "zip";
    // End of central directory: 22 bytes plus a comment of up to 65535.
    let floor = data.len().saturating_sub(22 + 65535);
    let eocd = (floor..=data.len().saturating_sub(22))
        .rev()
        .find(|i| data[*i..].starts_with(&[0x50, 0x4b, 5, 6]))
        .ok_or_else(|| bad(W, Reason::NotZip))?;
    let count = le16(data, eocd + 10);
    let mut at = le32(data, eocd + 16) as usize;
    for _ in 0..count {
        let h = data
            .get(at..at + 46)
            .ok_or_else(|| bad(W, Reason::ZipTruncatedDirectory))?;
        if h[..4] != [0x50, 0x4b, 1, 2] {
            return Err(bad(W, Reason::ZipDirectory));
        }
        let (flags, method) = (le16(h, 8), le16(h, 10));
        let crc = le32(h, 16);
        let (csize, usize_) = (le32(h, 20) as usize, le32(h, 24) as usize);
        let (nlen, xlen, clen) = (le16(h, 28), le16(h, 30), le16(h, 32));
        let local = le32(h, 42) as usize;
        let entry = data
            .get(at + 46..at + 46 + nlen)
            .ok_or_else(|| bad(W, Reason::ZipTruncatedName))?;
        at += 46 + nlen + xlen + clen;
        if entry != name.as_bytes() {
            continue;
        }
        if flags & 1 != 0 {
            return Err(bad(name, Reason::Encrypted));
        }
        if csize == 0xffff_ffff || usize_ == 0xffff_ffff || local == 0xffff_ffff {
            return Err(bad(name, Reason::Zip64));
        }
        if usize_ > limit {
            return Err(bad(name, Reason::TooLarge));
        }
        let lh = data
            .get(local..local + 30)
            .ok_or_else(|| bad(W, Reason::ZipTruncatedHeader))?;
        if lh[..4] != [0x50, 0x4b, 3, 4] {
            return Err(bad(name, Reason::BadLocalHeader));
        }
        let start = local + 30 + le16(lh, 26) + le16(lh, 28);
        let raw = data
            .get(start..start + csize)
            .ok_or_else(|| bad(W, Reason::ZipTruncatedData))?;
        let out = match method {
            0 => raw.to_vec(),
            8 => inflate(raw, limit, name)?,
            m => {
                return Err(bad(name, Reason::ZipMethod(m)));
            }
        };
        if out.len() != usize_ || crc32(&out) != crc {
            return Err(bad(name, Reason::Checksum));
        }
        return Ok(Some(out));
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A ustar header block for `name` (`kind` as tar's type flag), then the data blocks.
    pub(crate) fn entry(name: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        let mut out = h;
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    pub(crate) fn tar(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = entries.concat();
        out.extend_from_slice(&[0; 1024]);
        out
    }

    /// Minimal gzip writer (`miniz_oxide` deflate).
    pub(crate) fn gzip(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        out.extend(miniz_oxide::deflate::compress_to_vec(data, 6));
        out.extend_from_slice(&crc32(data).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    const ANY: &dyn Fn(&str, bool) -> bool = &|_, _| true;

    #[test]
    fn crc_and_gzip_round_trip() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let gz = gzip(b"hello hello hello");
        assert_eq!(gunzip(&gz, 1 << 20).unwrap(), b"hello hello hello");
        let mut broken = gz.clone();
        let n = broken.len();
        broken[n - 8] ^= 1;
        assert!(gunzip(&broken, 1 << 20).is_err());
        assert!(gunzip(&gz, 4).is_err(), "over the limit");
        assert!(gunzip(b"PK\x03\x04................", 1 << 20).is_err());
    }

    #[test]
    fn tar_reads_files_and_long_names() {
        let long = format!("assets/{}/x.css", "d".repeat(120));
        let pax_rec = format!("path={long}\n");
        let rec = format!("{} {pax_rec}", pax_rec.len() + 4);
        let t = tar(&[
            entry("./", b'5', b""),
            entry("worker/", b'5', b""),
            entry("worker/index.js", b'0', b"js"),
            entry("PaxHeaders/x", b'x', rec.as_bytes()),
            entry("ignored-short-name", b'0', b"css"),
        ]);
        let files = untar(&t, ANY).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files["worker/index.js"], b"js");
        assert_eq!(files[&long], b"css");
    }

    #[test]
    fn tar_refuses_escapes_links_and_devices() {
        for (name, kind) in [
            ("../evil", b'0'),
            ("worker/../../evil", b'0'),
            ("/etc/passwd", b'0'),
            ("C:/x", b'0'),
            ("a\\b", b'0'),
            ("worker/link", b'2'),
            ("worker/hard", b'1'),
            ("worker/dev", b'3'),
            ("worker/blk", b'4'),
            ("worker/fifo", b'6'),
        ] {
            let t = tar(&[entry(name, kind, b"x")]);
            assert!(untar(&t, ANY).is_err(), "{name} {kind}");
        }
        let dup = tar(&[entry("a", b'0', b"1"), entry("./a", b'0', b"2")]);
        assert!(untar(&dup, ANY).is_err());
        let nested = tar(&[entry("a", b'0', b"1"), entry("a/b", b'0', b"2")]);
        assert!(untar(&nested, ANY).is_err());
        let mut corrupt = tar(&[entry("a", b'0', b"1")]);
        corrupt[0] = b'b';
        assert_eq!(
            untar(&corrupt, ANY).unwrap_err().reason,
            Reason::HeaderChecksum
        );
        assert!(
            untar(&entry("a", b'0', b"1"), ANY).is_err(),
            "no end marker"
        );
    }

    /// A zip with one stored member, as the Windows release archive (blog.exe) would be.
    fn zip_stored(name: &str, data: &[u8]) -> Vec<u8> {
        let mut z = vec![0x50, 0x4b, 3, 4, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let crc = crc32(data).to_le_bytes();
        #[allow(clippy::cast_possible_truncation)]
        let len = (data.len() as u32).to_le_bytes();
        z.extend_from_slice(&crc);
        z.extend_from_slice(&len);
        z.extend_from_slice(&len);
        #[allow(clippy::cast_possible_truncation)]
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(name.as_bytes());
        z.extend_from_slice(data);
        let cd = z.len();
        z.extend_from_slice(&[0x50, 0x4b, 1, 2, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&crc);
        z.extend_from_slice(&len);
        z.extend_from_slice(&len);
        #[allow(clippy::cast_possible_truncation)]
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0; 12]);
        z.extend_from_slice(&[0; 4]);
        z.extend_from_slice(name.as_bytes());
        let cd_len = z.len() - cd;
        z.extend_from_slice(&[0x50, 0x4b, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0]);
        #[allow(clippy::cast_possible_truncation)]
        z.extend_from_slice(&(cd_len as u32).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        z.extend_from_slice(&(cd as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z
    }

    #[test]
    fn zip_finds_one_member() {
        let z = zip_stored("blog.exe", b"MZ binary");
        assert_eq!(
            unzip_one(&z, "blog.exe", 1 << 20).unwrap().unwrap(),
            b"MZ binary"
        );
        assert_eq!(unzip_one(&z, "other", 1 << 20).unwrap(), None);
        let mut bad = z.clone();
        bad[40] ^= 1;
        assert!(unzip_one(&bad, "blog.exe", 1 << 20).is_err());
    }
}
