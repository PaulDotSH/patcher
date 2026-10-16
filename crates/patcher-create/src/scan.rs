//! Walking the base and modded game folders, honouring ignore rules.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::WalkDir;

/// Junk that never belongs in a patch.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "Thumbs.db",
    "desktop.ini",
    ".DS_Store",
    ".git/",
    ".svn/",
    ".hg/",
    "*.log",
    ".patcherignore",
    ".patcher-backup/",
    ".patcher-staging/",
];

pub const IGNORE_FILE: &str = ".patcherignore";

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub abs: PathBuf,
    pub size: u64,
    pub exec: bool,
}

#[derive(Debug, Default)]
pub struct Tree {
    pub files: BTreeMap<String, FileEntry>,
    pub dirs: BTreeSet<String>,
    pub skipped_symlinks: Vec<String>,
}

pub struct Filter {
    gi: Gitignore,
}

impl Filter {
    /// Default excludes, then `ignore_files` (gitignore syntax), then `excludes` globs.
    pub fn new(excludes: &[String], ignore_files: &[PathBuf]) -> Result<Filter> {
        let mut b = GitignoreBuilder::new("");
        for p in DEFAULT_EXCLUDES {
            b.add_line(None, p)?;
        }
        for f in ignore_files {
            if let Some(e) = b.add(f) {
                return Err(e).with_context(|| format!("reading {}", f.display()));
            }
        }
        for p in excludes {
            b.add_line(None, p).with_context(|| format!("bad --exclude pattern {p:?}"))?;
        }
        Ok(Filter { gi: b.build()? })
    }

    pub fn ignored(&self, rel: &str, is_dir: bool) -> bool {
        self.gi.matched_path_or_any_parents(rel, is_dir).is_ignore()
    }
}

pub fn scan(root: &Path, filter: &Filter) -> Result<Tree> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let mut tree = Tree::default();
    let walker = WalkDir::new(root).follow_links(false).sort_by_file_name().into_iter();
    let it = walker.filter_entry(|e| {
        e.depth() == 0 || rel_of(root, e.path()).is_none_or(|r| !filter.ignored(&r, e.file_type().is_dir()))
    });
    for e in it {
        let e = e.with_context(|| format!("walking {}", root.display()))?;
        if e.depth() == 0 {
            continue;
        }
        let Some(rel) = rel_of(root, e.path()) else {
            bail!("{}: file names must be valid UTF-8", e.path().display());
        };
        let ft = e.file_type();
        if ft.is_symlink() {
            tree.skipped_symlinks.push(rel);
        } else if ft.is_dir() {
            tree.dirs.insert(rel);
        } else if ft.is_file() {
            let md = e.metadata()?;
            tree.files.insert(rel, FileEntry { abs: e.path().to_path_buf(), size: md.len(), exec: is_exec(&md) });
        }
    }
    Ok(tree)
}

fn rel_of(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

#[cfg(unix)]
fn is_exec(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_exec(_: &std::fs::Metadata) -> bool {
    false
}

/// Content sniffing for files authored on systems without an executable bit.
pub fn looks_executable(data: &[u8]) -> bool {
    data.starts_with(b"\x7fELF") || data.starts_with(b"#!")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_rules() {
        let f = Filter::new(&["*.bak".into(), "saves/".into()], &[]).unwrap();
        assert!(f.ignored("Thumbs.db", false));
        assert!(f.ignored("sub/Thumbs.db", false));
        assert!(f.ignored("logs/game.log", false));
        assert!(f.ignored(".git", true));
        assert!(f.ignored("a/b.bak", false));
        assert!(f.ignored("saves", true));
        assert!(f.ignored("saves/slot1.dat", false));
        assert!(!f.ignored("data/main.pak", false));
        assert!(!f.ignored("savestate.dat", false));
    }
}
