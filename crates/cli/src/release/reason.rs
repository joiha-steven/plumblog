//! Why a release file, archive or signature was refused. Each variant is one sentence in
//! `locales/<lang>.toml` (`cli.reason.*`); the data in a variant (a method number, a parser
//! message) is raw detail, never prose, so it stays out of the translated text.

use crate::text::Text;
use plumblog_i18n::{Key, key};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    // gzip
    NotGzip,
    GzipFlags,
    GzipHeaderTruncated,
    GzipChecksum,
    /// The decompressor's own status word.
    Inflate(String),
    // tar
    BadNumber,
    PathNotUtf8,
    PaxRecord,
    PaxLength,
    PaxSize,
    PaxLink,
    HardLink,
    Symlink,
    DeviceFile,
    Fifo,
    EntryType(char),
    NoEndMarker,
    HeaderChecksum,
    BinarySize,
    TooLarge,
    Truncated,
    UnsafePath,
    NotAllowed,
    FileAndDirectory,
    Duplicate,
    InsideFile,
    // zip
    NotZip,
    ZipDirectory,
    ZipTruncatedDirectory,
    ZipTruncatedName,
    ZipTruncatedHeader,
    ZipTruncatedData,
    Encrypted,
    Zip64,
    BadLocalHeader,
    ZipMethod(usize),
    Checksum,
    // bundle.json and the unpacked bundle
    Missing,
    /// A field `bundle.json` must have (`version`, `files`).
    ManifestField(&'static str),
    NotListed,
    HashDiffers,
    ListedNotInArchive,
    // the download folder, GitHub's reply
    NotSet,
    NoTagName,
    TagNotVersion,
    // signatures
    SignatureNotText,
    SumsNotText,
    SignatureBad,
    SignaturePrehashed,
    SigEmpty,
    SigNoCommentLine,
    SigNotBase64,
    SigWrongLength,
    SigNoTrustedComment,
    SigGlobalNotBase64,
    SigGlobalWrongLength,
    SigUnknownAlgorithm,
    /// A parser or OS message shown as it is (technical, not translated).
    Detail(String),
}

impl Reason {
    #[must_use]
    pub fn render(&self, text: &Text) -> String {
        let p = |k: Key| text.t(k, &[]);
        match self {
            Self::NotGzip => p(key!("cli.reason.not_gzip")),
            Self::GzipFlags => p(key!("cli.reason.gzip_flags")),
            Self::GzipHeaderTruncated => p(key!("cli.reason.gzip_header_truncated")),
            Self::GzipChecksum => p(key!("cli.reason.gzip_checksum")),
            Self::Inflate(status) => {
                text.t(key!("cli.reason.inflate"), &[("status", status.clone())])
            }
            Self::BadNumber => p(key!("cli.reason.bad_number")),
            Self::PathNotUtf8 => p(key!("cli.reason.path_not_utf8")),
            Self::PaxRecord => p(key!("cli.reason.pax_record")),
            Self::PaxLength => p(key!("cli.reason.pax_length")),
            Self::PaxSize => p(key!("cli.reason.pax_size")),
            Self::PaxLink => p(key!("cli.reason.pax_link")),
            Self::HardLink => p(key!("cli.reason.hard_link")),
            Self::Symlink => p(key!("cli.reason.symlink")),
            Self::DeviceFile => p(key!("cli.reason.device_file")),
            Self::Fifo => p(key!("cli.reason.fifo")),
            Self::EntryType(c) => text.t(key!("cli.reason.entry_type"), &[("type", c.to_string())]),
            Self::NoEndMarker => p(key!("cli.reason.no_end_marker")),
            Self::HeaderChecksum => p(key!("cli.reason.header_checksum")),
            Self::BinarySize => p(key!("cli.reason.binary_size")),
            Self::TooLarge => p(key!("cli.reason.too_large")),
            Self::Truncated => p(key!("cli.reason.truncated")),
            Self::UnsafePath => p(key!("cli.reason.unsafe_path")),
            Self::NotAllowed => p(key!("cli.reason.not_allowed")),
            Self::FileAndDirectory => p(key!("cli.reason.file_and_directory")),
            Self::Duplicate => p(key!("cli.reason.duplicate")),
            Self::InsideFile => p(key!("cli.reason.inside_file")),
            Self::NotZip => p(key!("cli.reason.not_zip")),
            Self::ZipDirectory => p(key!("cli.reason.zip_directory")),
            Self::ZipTruncatedDirectory => p(key!("cli.reason.zip_truncated_directory")),
            Self::ZipTruncatedName => p(key!("cli.reason.zip_truncated_name")),
            Self::ZipTruncatedHeader => p(key!("cli.reason.zip_truncated_header")),
            Self::ZipTruncatedData => p(key!("cli.reason.zip_truncated_data")),
            Self::Encrypted => p(key!("cli.reason.encrypted")),
            Self::Zip64 => p(key!("cli.reason.zip64")),
            Self::BadLocalHeader => p(key!("cli.reason.bad_local_header")),
            Self::ZipMethod(m) => {
                text.t(key!("cli.reason.zip_method"), &[("method", m.to_string())])
            }
            Self::Checksum => p(key!("cli.reason.checksum")),
            Self::Missing => p(key!("cli.reason.missing")),
            Self::ManifestField(field) => text.t(
                key!("cli.reason.manifest_field"),
                &[("field", (*field).to_string())],
            ),
            Self::NotListed => p(key!("cli.reason.not_listed")),
            Self::HashDiffers => p(key!("cli.reason.hash_differs")),
            Self::ListedNotInArchive => p(key!("cli.reason.listed_not_in_archive")),
            Self::NotSet => p(key!("cli.reason.not_set")),
            Self::NoTagName => p(key!("cli.reason.no_tag_name")),
            Self::TagNotVersion => p(key!("cli.reason.tag_not_version")),
            Self::SignatureNotText => p(key!("cli.reason.signature_not_text")),
            Self::SumsNotText => p(key!("cli.reason.sums_not_text")),
            Self::SignatureBad => p(key!("cli.reason.signature_bad")),
            Self::SignaturePrehashed => p(key!("cli.reason.signature_prehashed")),
            Self::SigEmpty => p(key!("cli.reason.sig_empty")),
            Self::SigNoCommentLine => p(key!("cli.reason.sig_no_comment_line")),
            Self::SigNotBase64 => p(key!("cli.reason.sig_not_base64")),
            Self::SigWrongLength => p(key!("cli.reason.sig_wrong_length")),
            Self::SigNoTrustedComment => p(key!("cli.reason.sig_no_trusted_comment")),
            Self::SigGlobalNotBase64 => p(key!("cli.reason.sig_global_not_base64")),
            Self::SigGlobalWrongLength => p(key!("cli.reason.sig_global_wrong_length")),
            Self::SigUnknownAlgorithm => p(key!("cli.reason.sig_unknown_algorithm")),
            Self::Detail(s) => s.clone(),
        }
    }
}
