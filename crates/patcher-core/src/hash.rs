//! Content checks: size + xxh3 (always) and BLAKE3 (when the author opted in).

use std::fs::File;
use std::ops::Deref;
use std::path::Path;

use memmap2::Mmap;
use xxhash_rust::xxh3::{xxh3_64, Xxh3};

use crate::format::Check;
use crate::{IoContext, Result};

pub fn check_of(data: &[u8], strong: bool) -> Check {
    Check { size: data.len() as u64, xxh3: xxh3_64(data), blake3: strong.then(|| blake3_of(data)) }
}

pub fn blake3_of(data: &[u8]) -> [u8; 32] {
    if data.len() >= 1 << 20 {
        *blake3::Hasher::new().update_rayon(data).finalize().as_bytes()
    } else {
        *blake3::hash(data).as_bytes()
    }
}

/// Verifies `data` against `expected`, hashing only what `expected` carries.
pub fn matches(expected: &Check, data: &[u8]) -> bool {
    data.len() as u64 == expected.size
        && xxh3_64(data) == expected.xxh3
        && expected.blake3.is_none_or(|h| blake3_of(data) == h)
}

/// Incremental version of `check_of`, for outputs produced in pieces.
pub struct Hasher {
    size: u64,
    xxh3: Xxh3,
    blake3: Option<blake3::Hasher>,
}

impl Hasher {
    pub fn new(strong: bool) -> Self {
        Hasher { size: 0, xxh3: Xxh3::new(), blake3: strong.then(blake3::Hasher::new) }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.size += data.len() as u64;
        self.xxh3.update(data);
        if let Some(b) = &mut self.blake3 {
            if data.len() >= 1 << 20 {
                b.update_rayon(data);
            } else {
                b.update(data);
            }
        }
    }

    pub fn finish(self) -> Check {
        Check {
            size: self.size,
            xxh3: self.xxh3.digest(),
            blake3: self.blake3.map(|b| *b.finalize().as_bytes()),
        }
    }
}

/// A read-only memory map that also works for empty files.
pub struct Mapped(Option<Mmap>);

impl Mapped {
    pub fn open(path: &Path) -> Result<Mapped> {
        let f = File::open(path).at(path)?;
        if f.metadata().at(path)?.len() == 0 {
            return Ok(Mapped(None));
        }
        // SAFETY: the map is only read; files changed underneath us by another process
        // are caught by the content checks.
        let m = unsafe { Mmap::map(&f) }.at(path)?;
        Ok(Mapped(Some(m)))
    }
}

impl Deref for Mapped {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.0.as_deref().unwrap_or(&[])
    }
}
