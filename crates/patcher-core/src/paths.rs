//! Mapping patch paths (UTF-8, `/`-separated) onto the real file system.
//!
//! Windows file systems are already case-insensitive. Elsewhere a game made on Windows
//! may reference `Data/Foo.png` while the disk has `data/foo.png`, so lookups fall back
//! to a case-insensitive match per component (ambiguous matches are an error).

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, IoContext, Result};

pub fn join(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Finds an existing file or directory for `rel`, or `None` if it does not exist.
pub fn resolve_existing(root: &Path, rel: &str) -> Result<Option<PathBuf>> {
    let exact = join(root, rel);
    if exact.symlink_metadata().is_ok() {
        return Ok(Some(exact));
    }
    if cfg!(windows) {
        return Ok(None);
    }
    let mut cur = root.to_path_buf();
    for comp in rel.split('/') {
        match find_ci(&cur, comp)? {
            Some(p) => cur = p,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// Where a file for `rel` should be written: existing ancestors are matched
/// case-insensitively (off Windows) so we never create `Data/` next to `data/`.
pub fn resolve_for_write(root: &Path, rel: &str) -> Result<PathBuf> {
    if cfg!(windows) {
        return Ok(join(root, rel));
    }
    let mut cur = root.to_path_buf();
    let mut comps = rel.split('/');
    for comp in comps.by_ref() {
        match find_ci(&cur, comp)? {
            Some(p) => cur = p,
            None => {
                cur.push(comp);
                break;
            }
        }
    }
    Ok(comps.fold(cur, |p, c| p.join(c)))
}

fn find_ci(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    let exact = dir.join(name);
    if exact.symlink_metadata().is_ok() {
        return Ok(Some(exact));
    }
    let Ok(rd) = fs::read_dir(dir) else { return Ok(None) };
    let want = name.to_lowercase();
    let mut found = None;
    for e in rd {
        let e = e.at(dir)?;
        if e.file_name().to_string_lossy().to_lowercase() == want {
            if found.is_some() {
                return Err(Error::Other(format!(
                    "{}: several entries match {name:?} ignoring case",
                    dir.display()
                )));
            }
            found = Some(e.path());
        }
    }
    Ok(found)
}

/// `path` relative to `root`, as a patch path.
pub fn to_rel(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_exact_and_missing() {
        let dir = std::env::temp_dir().join(format!("patcher-paths-{}", std::process::id()));
        fs::create_dir_all(dir.join("Data")).unwrap();
        fs::write(dir.join("Data/Foo.png"), b"x").unwrap();
        assert!(resolve_existing(&dir, "Data/Foo.png").unwrap().is_some());
        assert!(resolve_existing(&dir, "Data/Bar.png").unwrap().is_none());
        // Case-insensitive on every platform (natively on Windows, by fallback elsewhere).
        let found = resolve_existing(&dir, "data/foo.png").unwrap().unwrap();
        assert_eq!(fs::read(found).unwrap(), b"x");
        let w = resolve_for_write(&dir, "data/new/thing.txt").unwrap();
        assert!(to_rel(&dir, &w).ends_with("new/thing.txt"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
