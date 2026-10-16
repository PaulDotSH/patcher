//! The UI bundle: the theme's folder, packed into the patcher executable.
//!
//! Wire format: `"PUI1"`, the decoded length (u64 LE), then a zstd frame holding a
//! bitcode-encoded [`Raw`]. The whole folder is packed (hidden files excepted), so the
//! theme's `.slint` imports and `@image-url` paths resolve after extraction exactly as they
//! do in the author's folder.

use std::path::{Path, PathBuf};

use bitcode::{Decode, Encode};
use patcher_core::format::{validate_rel_path, Comp};

const MAGIC: [u8; 4] = *b"PUI1";
pub const MAX_FILES: usize = 1024;
pub const MAX_BYTES: u64 = 64 << 20;

#[derive(Debug, thiserror::Error)]
#[error("UI bundle: {0}")]
pub struct BundleError(pub String);

type Result<T> = std::result::Result<T, BundleError>;

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Bundle {
    /// Bundle path of the theme file.
    pub theme: String,
    /// Bundle path (relative, `/`-separated) and contents of every file.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Packs the folder holding `theme_file`.
#[cfg(feature = "pack")]
pub fn pack(theme_file: &Path) -> Result<Vec<u8>> {
    let err = |e: std::io::Error, p: &Path| BundleError(format!("{}: {e}", p.display()));
    let dir = theme_file.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d).map_err(|e| err(e, &d))?.collect::<std::io::Result<_>>().map_err(|e| err(e, &d))?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = rel_path(dir, &p)?;
            let data = std::fs::read(&p).map_err(|e| err(e, &p))?;
            total += data.len() as u64;
            files.push((rel, data));
            if files.len() > MAX_FILES || total > MAX_BYTES {
                return Err(BundleError(format!(
                    "the theme folder {} holds more than {MAX_FILES} files or {} MiB; keep the theme in a folder of its own",
                    dir.display(),
                    MAX_BYTES >> 20
                )));
            }
        }
    }
    let bundle = Bundle { theme: rel_path(dir, theme_file)?, files };
    let raw = bitcode::encode(&bundle);
    let packed = zstd::bulk::compress(&raw, 19).map_err(|e| BundleError(e.to_string()))?;
    let mut out = Vec::with_capacity(packed.len() + 12);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(raw.len() as u64).to_le_bytes());
    out.extend_from_slice(&packed);
    Ok(out)
}

#[cfg(feature = "pack")]
fn rel_path(dir: &Path, p: &Path) -> Result<String> {
    let rel = p.strip_prefix(dir).map_err(|_| BundleError(format!("{} is outside {}", p.display(), dir.display())))?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let s = parts.join("/");
    validate_rel_path(&s).map_err(|e| BundleError(format!("{s}: {e}")))?;
    Ok(s)
}

pub fn decode(bytes: &[u8]) -> Result<Bundle> {
    if bytes.len() < 12 || bytes[..4] != MAGIC {
        return Err(BundleError("not a UI bundle".into()));
    }
    let raw_len = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
    // Paths and length prefixes add a little on top of the file data.
    if raw_len > MAX_BYTES + (1 << 20) {
        return Err(BundleError(format!("declares {raw_len} bytes, over the limit")));
    }
    let raw = patcher_core::codec::decompress(Comp::Zstd, &bytes[12..], None, raw_len as usize)
        .map_err(|e| BundleError(e.to_string()))?;
    let b: Bundle = bitcode::decode(&raw).map_err(|e| BundleError(format!("corrupt: {e}")))?;
    if b.files.len() > MAX_FILES {
        return Err(BundleError(format!("{} files, over the limit of {MAX_FILES}", b.files.len())));
    }
    for (p, _) in &b.files {
        validate_rel_path(p).map_err(|e| BundleError(format!("{p}: {e}")))?;
    }
    if !b.files.iter().any(|(p, _)| *p == b.theme) {
        return Err(BundleError("the theme file is missing".into()));
    }
    Ok(b)
}

/// Extracts `bytes` into `<root>/patcher-ui-<hash>/` (reused when already complete) and
/// returns the path of the theme file inside it.
pub fn extract(bytes: &[u8], root: &Path) -> Result<PathBuf> {
    let b = decode(bytes)?;
    let dir = root.join(format!("patcher-ui-{:016x}", xxhash_rust::xxh3::xxh3_64(bytes)));
    let done = dir.join(".complete");
    if !done.is_file() {
        let err = |e: std::io::Error, p: &Path| BundleError(format!("{}: {e}", p.display()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, data) in &b.files {
            let p = patcher_core::paths::join(&dir, rel);
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| err(e, &p))?;
            std::fs::write(&p, data).map_err(|e| err(e, &p))?;
        }
        std::fs::write(&done, b"").map_err(|e| err(e, &done))?;
    }
    Ok(patcher_core::paths::join(&dir, &b.theme))
}

#[cfg(all(test, feature = "pack"))]
mod tests {
    use super::*;

    fn folder(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("patcher-gui-bundle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, d) in [("theme.toml", &b"[theme]\naccent = \"#ff0000\"\n"[..]), ("ui/a.slint", b"// a"), ("img/x.png", b"png"), (".git/HEAD", b"ignored")] {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, d).unwrap();
        }
        dir
    }

    #[test]
    fn round_trip_and_extract() {
        let dir = folder("rt");
        let bytes = pack(&dir.join("theme.toml")).unwrap();
        let b = decode(&bytes).unwrap();
        assert_eq!(b.theme, "theme.toml");
        let mut names: Vec<_> = b.files.iter().map(|f| f.0.as_str()).collect();
        names.sort();
        assert_eq!(names, ["img/x.png", "theme.toml", "ui/a.slint"]);

        let root = dir.join("out");
        let theme = extract(&bytes, &root).unwrap();
        assert_eq!(std::fs::read(&theme).unwrap(), std::fs::read(dir.join("theme.toml")).unwrap());
        assert_eq!(std::fs::read(theme.parent().unwrap().join("ui/a.slint")).unwrap(), b"// a");
        // Reused when complete.
        assert_eq!(extract(&bytes, &root).unwrap(), theme);
        let t = crate::Theme::load(&theme).unwrap();
        assert_eq!(t.palette["accent"], crate::color::Color::rgb(0xff0000));
    }

    #[test]
    fn hostile_bundles_are_rejected() {
        let enc = |b: &Bundle| {
            let raw = bitcode::encode(b);
            let mut out = MAGIC.to_vec();
            out.extend_from_slice(&(raw.len() as u64).to_le_bytes());
            out.extend_from_slice(&zstd::bulk::compress(&raw, 1).unwrap());
            out
        };
        let traversal = Bundle { theme: "t.toml".into(), files: vec![("t.toml".into(), vec![]), ("../evil".into(), vec![])] };
        assert!(decode(&enc(&traversal)).unwrap_err().0.contains("../evil"));
        let no_theme = Bundle { theme: "t.toml".into(), files: vec![("x".into(), vec![])] };
        assert!(decode(&enc(&no_theme)).is_err());
        let mut huge = enc(&no_theme);
        huge[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&huge).unwrap_err().0.contains("limit"));
        assert!(decode(b"nope").is_err());
    }

    #[test]
    fn too_many_files() {
        let dir = folder("many");
        for i in 0..=MAX_FILES {
            std::fs::write(dir.join(format!("f{i}.txt")), b"").unwrap();
        }
        assert!(pack(&dir.join("theme.toml")).unwrap_err().0.contains("folder of its own"));
    }
}
