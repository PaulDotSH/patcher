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
//! A standalone patcher is `stub executable ++ patch ++ Footer (24 bytes)`.

use bitcode::{Decode, Encode};

use crate::{Error, Result};

pub const MAGIC: [u8; 8] = *b"PTCHDB\0\x01";
pub const FORMAT_VERSION: u32 = 1;
pub const HEADER_LEN: usize = 48;
pub const FOOTER_MAGIC: [u8; 8] = *b"PTCHEND1";
pub const FOOTER_LEN: usize = 24;

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

/// Trailer appended after a patch embedded in a stub executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footer {
    pub patch_offset: u64,
    pub patch_len: u64,
}

impl Footer {
    pub fn to_bytes(&self) -> [u8; FOOTER_LEN] {
        let mut b = [0u8; FOOTER_LEN];
        b[0..8].copy_from_slice(&self.patch_offset.to_le_bytes());
        b[8..16].copy_from_slice(&self.patch_len.to_le_bytes());
        b[16..24].copy_from_slice(&FOOTER_MAGIC);
        b
    }

    /// Looks for a footer at the end of `file`.
    pub fn find(file: &[u8]) -> Option<Footer> {
        let tail = file.get(file.len().checked_sub(FOOTER_LEN)?..)?;
        if tail[16..24] != FOOTER_MAGIC {
            return None;
        }
        let f = Footer {
            patch_offset: u64::from_le_bytes(tail[0..8].try_into().unwrap()),
            patch_len: u64::from_le_bytes(tail[8..16].try_into().unwrap()),
        };
        let end = f.patch_offset.checked_add(f.patch_len)?;
        (end <= (file.len() - FOOTER_LEN) as u64).then_some(f)
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

    #[test]
    fn footer_found_only_when_valid() {
        let mut file = vec![7u8; 100];
        file.extend_from_slice(&Footer { patch_offset: 10, patch_len: 90 }.to_bytes());
        assert_eq!(Footer::find(&file), Some(Footer { patch_offset: 10, patch_len: 90 }));
        let mut bad = vec![7u8; 100];
        bad.extend_from_slice(&Footer { patch_offset: 10, patch_len: 91 }.to_bytes());
        assert_eq!(Footer::find(&bad), None);
        assert_eq!(Footer::find(&[1, 2, 3]), None);
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
