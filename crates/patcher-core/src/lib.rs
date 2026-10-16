//! Patcher core: the patch format, pure-Rust decoders and the apply/revert engine.
//!
//! Everything here must stay pure Rust: the end-user stub links only this crate so it
//! can be cross-compiled for every platform from one machine.

pub mod apply;
pub mod bcj;
pub mod codec;
pub mod format;
pub mod hash;
pub mod patch;
pub mod paths;
pub mod progress;
pub mod sys;
pub mod ui;
pub mod varint;

use std::path::PathBuf;

pub use apply::{apply, apply_to_copy, inspect, revert, ApplyOptions, ApplyReport, GameState};
pub use patch::Patch;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a patch file")]
    NotAPatch,
    #[error("unsupported patch format version {0} (this patcher reads version {v})", v = format::FORMAT_VERSION)]
    UnsupportedVersion(u32),
    #[error("corrupt patch: {0}")]
    Corrupt(String),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("the game files do not match the version this patch was made for:\n  {}", .0.join("\n  "))]
    BaseMismatch(Vec<String>),
    #[error("this patch is already applied")]
    AlreadyApplied,
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Attaches a path to I/O errors.
pub trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::Io { path: path.into(), source })
    }
}
