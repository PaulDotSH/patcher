//! Opening a patch: a plain `.patch` file or one embedded in a standalone patcher.

use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::apply::{BACKUP_DIR, STAGING_DIR};
use crate::format::{validate_rel_path, Blob, Comp, Footer, Header, Index, Source, MAGIC};
use crate::{codec, Error, IoContext, Result};

/// Upper bound for the decoded index, so a corrupt header cannot request absurd allocations.
const MAX_INDEX_LEN: u64 = 1 << 30;
/// Upper bound for one decoded segment or solid group (the creator chunks large files).
const MAX_DECODED_LEN: u64 = 1 << 36;

enum Backing {
    Map(Mmap),
    Owned(Vec<u8>),
}

impl Backing {
    fn bytes(&self) -> &[u8] {
        match self {
            Backing::Map(m) => m,
            Backing::Owned(v) => v,
        }
    }
}

pub struct Patch {
    backing: Backing,
    start: usize,
    pub header: Header,
    pub index: Index,
}

impl Patch {
    /// Opens a `.patch` file or a standalone patcher executable.
    pub fn open(path: &Path) -> Result<Patch> {
        let f = File::open(path).at(path)?;
        // SAFETY: read-only map; the patch is validated before use and blob reads are
        // bounds-checked.
        let map = unsafe { Mmap::map(&f) }.at(path)?;
        Patch::from_backing(Backing::Map(map), true)
    }

    /// Opens the patch appended to `exe`, if there is one.
    pub fn open_embedded(exe: &Path) -> Result<Option<Patch>> {
        let f = File::open(exe).at(exe)?;
        // SAFETY: as in `open`.
        let map = unsafe { Mmap::map(&f) }.at(exe)?;
        if Footer::find(&map).and_then(|f| f.patch).is_none() {
            return Ok(None);
        }
        Patch::from_backing(Backing::Map(map), false).map(Some)
    }

    pub fn from_vec(data: Vec<u8>) -> Result<Patch> {
        Patch::from_backing(Backing::Owned(data), true)
    }

    fn from_backing(backing: Backing, allow_plain: bool) -> Result<Patch> {
        let data = backing.bytes();
        let (start, len) = if allow_plain && data.starts_with(&MAGIC) {
            (0, data.len())
        } else if let Some(r) = Footer::find(data).and_then(|f| f.patch) {
            (r.offset as usize, r.len as usize)
        } else {
            return Err(Error::NotAPatch);
        };
        let bytes = &data[start..start + len];
        let header = Header::parse(bytes)?;
        let index_end = header.index_offset.checked_add(header.index_len);
        if header.total_len > len as u64
            || index_end.is_none_or(|e| e > header.total_len)
            || header.index_raw_len > MAX_INDEX_LEN
        {
            return Err(Error::Corrupt("header points outside the patch".into()));
        }
        let raw_index = &bytes[header.index_offset as usize..index_end.unwrap() as usize];
        let raw = codec::decompress(Comp::Zstd, raw_index, None, header.index_raw_len as usize)?;
        let index = Index::decode(&raw)?;
        let patch = Patch { start, header, index, backing };
        patch.validate()?;
        Ok(patch)
    }

    /// The patch bytes (without any stub executable around them).
    pub fn bytes(&self) -> &[u8] {
        &self.backing.bytes()[self.start..self.start + self.header.total_len as usize]
    }

    pub fn blob(&self, b: Blob) -> Result<&[u8]> {
        let end = b.offset.checked_add(b.len).filter(|&e| e <= self.header.index_offset);
        match end {
            Some(e) if b.offset >= crate::format::HEADER_LEN as u64 => {
                Ok(&self.bytes()[b.offset as usize..e as usize])
            }
            _ => Err(Error::Corrupt("blob outside the data section".into())),
        }
    }

    fn validate(&self) -> Result<()> {
        let idx = &self.index;
        let bad = |m: &str| Err(Error::Corrupt(m.into()));
        let n_base = idx.base_files.len();
        for p in idx.base_files.iter().map(|b| &b.path).chain(idx.writes.iter().map(|w| &w.path)) {
            validate_rel_path(p)?;
        }
        for p in idx.dirs_create.iter().chain(&idx.dirs_remove) {
            validate_rel_path(p)?;
        }
        if idx.deletes.iter().any(|&d| d as usize >= n_base) {
            return bad("delete refers to an unknown base file");
        }
        let all = idx.base_files.iter().map(|b| &b.path).chain(idx.writes.iter().map(|w| &w.path));
        for p in all.chain(&idx.dirs_create).chain(&idx.dirs_remove) {
            let first = p.split('/').next().unwrap_or_default();
            if first.eq_ignore_ascii_case(BACKUP_DIR) || first.eq_ignore_ascii_case(STAGING_DIR) {
                return Err(Error::Corrupt(format!("reserved path in patch: {p:?}")));
            }
        }
        let mut written = HashSet::new();
        if !idx.writes.iter().all(|w| written.insert(w.path.to_lowercase())) {
            return bad("several outputs share a path");
        }
        if idx.deletes.iter().any(|&d| written.contains(&idx.base_files[d as usize].path.to_lowercase())) {
            return bad("a path is both written and deleted");
        }
        for g in &idx.solid {
            self.blob(g.blob)?;
            if g.len > MAX_DECODED_LEN {
                return bad("solid group too large");
            }
        }
        for w in &idx.writes {
            match &w.source {
                Source::Base(b) if *b as usize >= n_base => return bad("unknown base file"),
                Source::Base(b) if idx.base_files[*b as usize].check.size != w.check.size => {
                    return bad("copy size mismatch");
                }
                Source::Base(_) => {}
                Source::Solid { group, offset } => {
                    let Some(g) = idx.solid.get(*group as usize) else { return bad("unknown solid group") };
                    if offset.checked_add(w.check.size).is_none_or(|e| e > g.len) {
                        return bad("solid slice out of range");
                    }
                }
                Source::Segments(segs) => {
                    if segs.iter().any(|s| s.len > MAX_DECODED_LEN) {
                        return bad("segment too large");
                    }
                    if segs.iter().try_fold(0u64, |a, s| a.checked_add(s.len)) != Some(w.check.size) {
                        return bad("segment lengths do not add up");
                    }
                    for s in segs {
                        self.blob(s.blob)?;
                        match (s.codec.needs_base(), s.base) {
                            (true, None) => return bad("segment misses its base"),
                            (_, Some(bs)) => {
                                let Some(bf) = idx.base_files.get(bs.file as usize) else {
                                    return bad("unknown base file");
                                };
                                if bs.offset.checked_add(bs.len).is_none_or(|e| e > bf.check.size) {
                                    return bad("base slice out of range");
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
