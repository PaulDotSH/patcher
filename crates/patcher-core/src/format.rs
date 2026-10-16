//! On-disk layout of a patch ("patch db").
//!
//! ```text
//! +--------------------+  offset 0 (relative to the start of the patch)
//! | Header (48 bytes)  |  magic, format version, index location, total length
//! +--------------------+
//! | blobs ...          |  independent compressed payloads, addressed by `Blob`
//! +--------------------+
//! | index              |  bitcode-encoded `Index`, zstd-compressed
//! +--------------------+
//! ```
//!
//! A standalone patcher is `stub executable ++ [UI bundle] ++ [patch] ++ Footer (40 bytes)`.
//! Older patchers end in a 24-byte v1 footer that only locates a patch; it is still read.

use bitcode::{Decode, Encode};

use crate::{Error, Result};

pub const MAGIC: [u8; 8] = *b"PTCHDB\0\x01";
pub const FORMAT_VERSION: u32 = 1;
pub const HEADER_LEN: usize = 48;
pub const FOOTER_MAGIC: [u8; 8] = *b"PTCHEND2";
pub const FOOTER_LEN: usize = 40;
pub const FOOTER_V1_MAGIC: [u8; 8] = *b"PTCHEND1";
pub const FOOTER_V1_LEN: usize = 24;

/// Fixed-size header at the start of every patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub flags: u32,
    pub index_offset: u64,
    pub index_len: u64,
    pub index_raw_len: u64,
    pub total_len: u64,
}

impl Header {
    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..12].copy_from_slice(&self.version.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..24].copy_from_slice(&self.index_offset.to_le_bytes());
        b[24..32].copy_from_slice(&self.index_len.to_le_bytes());
        b[32..40].copy_from_slice(&self.index_raw_len.to_le_bytes());
        b[40..48].copy_from_slice(&self.total_len.to_le_bytes());
        b
    }

    pub fn parse(b: &[u8]) -> Result<Header> {
        if b.len() < HEADER_LEN || b[0..8] != MAGIC {
            return Err(Error::NotAPatch);
        }
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let h = Header {
            version: u32_at(8),
            flags: u32_at(12),
            index_offset: u64_at(16),
            index_len: u64_at(24),
            index_raw_len: u64_at(32),
            total_len: u64_at(40),
        };
        if h.version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(h.version));
        }
        Ok(h)
    }
}

/// A byte range inside a stub executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub offset: u64,
    pub len: u64,
}

impl Range {
    fn end(&self) -> Option<u64> {
        self.offset.checked_add(self.len)
    }
}

/// Trailer of a stub executable: where its UI bundle and its patch are, if it has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Footer {
    pub ui: Option<Range>,
    pub patch: Option<Range>,
}

impl Footer {
    pub fn to_bytes(&self) -> [u8; FOOTER_LEN] {
        let mut b = [0u8; FOOTER_LEN];
        for (i, r) in [self.ui, self.patch].into_iter().enumerate() {
            let r = r.unwrap_or(Range { offset: 0, len: 0 });
            b[i * 16..i * 16 + 8].copy_from_slice(&r.offset.to_le_bytes());
            b[i * 16 + 8..i * 16 + 16].copy_from_slice(&r.len.to_le_bytes());
        }
        b[32..40].copy_from_slice(&FOOTER_MAGIC);
        b
    }

    /// Looks for a v2 or v1 footer at the end of `file`. Ranges must lie before the footer
    /// and must not overlap.
    pub fn find(file: &[u8]) -> Option<Footer> {
        let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let range = |b: &[u8], at: usize| {
            let r = Range { offset: u64_at(b, at), len: u64_at(b, at + 8) };
            (r.len > 0).then_some(r)
        };
        let (f, body) = if let Some(tail) = file.get(file.len().checked_sub(FOOTER_LEN)?..)
            && tail[32..40] == FOOTER_MAGIC
        {
            (Footer { ui: range(tail, 0), patch: range(tail, 16) }, file.len() - FOOTER_LEN)
        } else {
            let tail = file.get(file.len().checked_sub(FOOTER_V1_LEN)?..)?;
            if tail[16..24] != FOOTER_V1_MAGIC {
                return None;
            }
            let patch = Range { offset: u64_at(tail, 0), len: u64_at(tail, 8) };
            (Footer { ui: None, patch: Some(patch) }, file.len() - FOOTER_V1_LEN)
        };
        for r in [f.ui, f.patch].into_iter().flatten() {
            if r.end()? > body as u64 {
                return None;
            }
        }
        if let (Some(a), Some(b)) = (f.ui, f.patch)
            && a.offset < b.end()?
            && b.offset < a.end()?
        {
            return None;
        }
        Some(f)
    }

    /// The UI bundle of a stub executable, if it carries one.
    pub fn ui_bytes(file: &[u8]) -> Option<&[u8]> {
        let r = Footer::find(file)?.ui?;
        file.get(r.offset as usize..r.end()? as usize)
    }
}

/// Everything the patcher needs to know, stored compressed at the end of the patch.
#[derive(Debug, Clone, Default, Encode, Decode, PartialEq)]
pub struct Index {
    pub meta: Meta,
    /// BLAKE3 over the sorted (path, size, xxh3) list of every base file the author scanned.
    pub base_fingerprint: [u8; 32],
    /// Whether `Check::blake3` is filled in (author chose hash verification).
    pub strong_hashes: bool,
    /// Every base file the patch reads, overwrites or deletes.
    pub base_files: Vec<BaseFile>,
    pub writes: Vec<WriteOp>,
    /// Indices into `base_files` that are removed.
    pub deletes: Vec<u32>,
    pub solid: Vec<SolidGroup>,
    /// Empty directories that exist only in the modded game.
    pub dirs_create: Vec<String>,
    /// Directories that exist only in the base game (removed when empty).
    pub dirs_remove: Vec<String>,
}

#[derive(Debug, Clone, Default, Encode, Decode, PartialEq, Eq)]
pub struct Meta {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub url: String,
    /// Tool that produced the patch, e.g. "patcher 0.1.0".
    pub created_by: String,
}

/// Identity of a file's contents. `size` + `xxh3` is always checked (cheap); `blake3`
/// only when the author enabled strong hashes.
#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq)]
pub struct Check {
    pub size: u64,
    pub xxh3: u64,
    pub blake3: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Encode, Decode, PartialEq, Eq)]
pub struct BaseFile {
    pub path: String,
    pub check: Check,
}

#[derive(Debug, Clone, Encode, Decode, PartialEq)]
pub struct WriteOp {
    pub path: String,
    pub check: Check,
    /// Unix executable bit for files the patch creates.
    pub exec: bool,
    pub source: Source,
}

#[derive(Debug, Clone, Encode, Decode, PartialEq)]
pub enum Source {
    /// Byte-identical to a base file (unchanged content at a new path).
    Base(u32),
    /// Concatenation of decoded segments.
    Segments(Vec<Segment>),
    /// `check.size` bytes at `offset` in a decoded solid group.
    Solid { group: u32, offset: u64 },
}

#[derive(Debug, Clone, Encode, Decode, PartialEq)]
pub struct Segment {
    /// Decoded length.
    pub len: u64,
    pub blob: Blob,
    pub codec: Codec,
    pub transform: Transform,
    /// Reference data for `Codec::Ref` and `Codec::Bsdiff`.
    pub base: Option<BaseSlice>,
}

#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq)]
pub struct Blob {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq)]
pub struct BaseSlice {
    pub file: u32,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq, Hash)]
pub enum Comp {
    Zstd,
    Lzma2 { dict_size: u32 },
}

#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Stored as is.
    Raw,
    /// Compressed without reference data.
    Full(Comp),
    /// Compressed with the base slice as history (zstd `--patch-from` / LZMA2 preset dictionary).
    Ref(Comp),
    /// bsdiff-style control/diff/extra streams, compressed with `Comp`.
    Bsdiff(Comp),
}

/// Reversible pre-filter applied to the base slice and the output of a segment.
#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq, Hash)]
pub enum Transform {
    None,
    /// x86 CALL/JMP rel32 -> absolute conversion (helps when code shifts).
    BcjX86,
}

#[derive(Debug, Clone, Copy, Encode, Decode, PartialEq, Eq)]
pub struct SolidGroup {
    pub blob: Blob,
    pub comp: Comp,
    pub len: u64,
}

impl Index {
    pub fn encode(&self) -> Vec<u8> {
        bitcode::encode(self)
    }

    pub fn decode(raw: &[u8]) -> Result<Index> {
        bitcode::decode(raw).map_err(|e| Error::Corrupt(format!("index: {e}")))
    }
}

impl Codec {
    /// Rough relative decode cost per output byte; used to break near-ties in size.
    pub fn decode_cost(self) -> u32 {
        match self {
            Codec::Raw => 0,
            Codec::Full(Comp::Zstd) | Codec::Ref(Comp::Zstd) => 10,
            Codec::Bsdiff(Comp::Zstd) => 20,
            Codec::Full(Comp::Lzma2 { .. }) | Codec::Ref(Comp::Lzma2 { .. }) => 40,
            Codec::Bsdiff(Comp::Lzma2 { .. }) => 50,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::Raw => "raw",
            Codec::Full(Comp::Zstd) => "zstd",
            Codec::Full(Comp::Lzma2 { .. }) => "lzma2",
            Codec::Ref(Comp::Zstd) => "zstd-ref",
            Codec::Ref(Comp::Lzma2 { .. }) => "lzma2-ref",
            Codec::Bsdiff(Comp::Zstd) => "bsdiff+zstd",
            Codec::Bsdiff(Comp::Lzma2 { .. }) => "bsdiff+lzma2",
        }
    }

    pub fn needs_base(self) -> bool {
        matches!(self, Codec::Ref(_) | Codec::Bsdiff(_))
    }
}

impl Transform {
    pub fn decode_cost(self) -> u32 {
        match self {
            Transform::None => 0,
            Transform::BcjX86 => 3,
        }
    }
}

/// Rejects paths that could escape the game directory (or, on Windows, name a device).
pub fn validate_rel_path(p: &str) -> Result<()> {
    let bad = p.is_empty()
        || p.starts_with('/')
        || p.contains('\\')
        || p.contains('\0')
        || p.contains(':')
        || p.split('/').any(|c| c.is_empty() || c == "." || c == ".." || (cfg!(windows) && bad_on_windows(c)));
    if bad {
        return Err(Error::Corrupt(format!("unsafe path in patch: {p:?}")));
    }
    Ok(())
}

/// Device names (`CON`, `nul.txt`, `COM1`...) and names Windows silently trims.
fn bad_on_windows(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    device || c.ends_with('.') || c.ends_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header {
            version: FORMAT_VERSION,
            flags: 3,
            index_offset: 1234,
            index_len: 99,
            index_raw_len: 300,
            total_len: 1333,
        };
        assert_eq!(Header::parse(&h.to_bytes()).unwrap(), h);
    }

    fn with_footer(f: Footer) -> Vec<u8> {
        let mut file = vec![7u8; 100];
        file.extend_from_slice(&f.to_bytes());
        file
    }

    #[test]
    fn footer_found_only_when_valid() {
        let r = |offset, len| Some(Range { offset, len });
        for f in [
            Footer { ui: None, patch: r(10, 90) },
            Footer { ui: r(10, 20), patch: r(30, 70) },
            Footer { ui: r(10, 90), patch: None },
        ] {
            assert_eq!(Footer::find(&with_footer(f)), Some(f), "{f:?}");
        }
        let bad = [
            ("past the end", Footer { ui: None, patch: r(10, 91) }),
            ("overlap", Footer { ui: r(10, 21), patch: r(30, 70) }),
            ("overflow", Footer { ui: r(u64::MAX, 2), patch: None }),
        ];
        for (what, f) in bad {
            assert_eq!(Footer::find(&with_footer(f)), None, "{what}");
        }
        assert_eq!(Footer::find(&[1, 2, 3]), None);
        let file = with_footer(Footer { ui: r(10, 20), patch: None });
        assert_eq!(Footer::ui_bytes(&file), Some(&file[10..30]));
    }

    #[test]
    fn v1_footer_still_read() {
        let mut file = vec![7u8; 100];
        file.extend_from_slice(&10u64.to_le_bytes());
        file.extend_from_slice(&90u64.to_le_bytes());
        file.extend_from_slice(&FOOTER_V1_MAGIC);
        assert_eq!(Footer::find(&file), Some(Footer { ui: None, patch: Some(Range { offset: 10, len: 90 }) }));
        file[108] = 91;
        assert_eq!(Footer::find(&file), None);
    }

    #[test]
    fn path_validation() {
        for ok in ["a", "a/b.txt", "data/main.pak", "x y/z"] {
            validate_rel_path(ok).unwrap();
        }
        for bad in ["", "/a", "a/../b", "..", "a//b", "C:/x", "a\\b", "./a", "a/"] {
            assert!(validate_rel_path(bad).is_err(), "{bad}");
        }
        for win in ["CON", "a/nul.txt", "Com1", "lpt9.tar.gz", "a./b", "a /b"] {
            assert_eq!(validate_rel_path(win).is_err(), cfg!(windows), "{win}");
        }
        validate_rel_path("console/com10/lpt.txt").unwrap();
    }
}
