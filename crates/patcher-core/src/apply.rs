//! Applying and reverting patches.
//!
//! Apply runs in three phases so a failure never leaves a half-patched game:
//! 1. verify every base file the patch touches (size + xxh3, plus BLAKE3 if present);
//! 2. decode all outputs into a staging directory inside the game folder, verifying each;
//! 3. commit with renames: originals move into `.patcher-backup/` (unless disabled), staged
//!    files move into place. Errors during the commit roll the renames back.
//!
//! Without a backup (`--no-backup`) the commit is not atomic: overwritten and deleted
//! originals are gone as soon as they are replaced, so a failure mid-commit can leave a
//! partly patched game. Only newly added files and directories are rolled back.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use bitcode::{Decode, Encode};
use rayon::prelude::*;

use crate::format::{validate_rel_path, Check, Codec, Comp, Index, Source};
use crate::hash::{self, Mapped};
use crate::progress::Progress;
use crate::sys::{self, Budget};
use crate::{codec, paths, Error, IoContext, Patch, Result};

pub const BACKUP_DIR: &str = ".patcher-backup";
pub const STAGING_DIR: &str = ".patcher-staging";
const MANIFEST: &str = "manifest.bin";

pub struct ApplyOptions<'a> {
    /// Keep the replaced and deleted originals so `revert` can restore them.
    pub backup: bool,
    pub progress: &'a dyn Progress,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub written: usize,
    pub written_bytes: u64,
    pub deleted: usize,
    pub backed_up: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameState {
    /// Base files match: the patch can be applied.
    Unpatched,
    /// Every output of the patch is already in place.
    Patched,
    /// Neither; lists what differs from the expected base.
    Mismatch(Vec<String>),
}

/// What `revert` needs, stored in the backup directory.
#[derive(Debug, Default, Encode, Decode)]
struct Manifest {
    patch_name: String,
    patch_version: String,
    /// Original files moved into the backup (overwritten or deleted by the patch).
    replaced: Vec<String>,
    /// Files the patch created.
    added: Vec<String>,
    dirs_created: Vec<String>,
    dirs_removed: Vec<String>,
}

/// Verified base files: resolved paths and their memory maps (same order as `base_files`).
struct BaseFiles {
    paths: Vec<PathBuf>,
    maps: Vec<Mapped>,
}

/// Reports whether `patch` can be applied to `game`, is already applied, or neither.
pub fn inspect(patch: &Patch, game: &Path, progress: &dyn Progress) -> Result<GameState> {
    match verify_base(&patch.index, game, progress)? {
        Ok(_) if patched_over_base(&patch.index, game)? => Ok(GameState::Patched),
        Ok(_) => Ok(GameState::Unpatched),
        Err(_) if is_patched(&patch.index, game)? => Ok(GameState::Patched),
        Err(list) => Ok(GameState::Mismatch(list)),
    }
}

pub fn apply(patch: &Patch, game: &Path, opts: &ApplyOptions) -> Result<ApplyReport> {
    let idx = &patch.index;
    let backup_root = game.join(BACKUP_DIR);
    if opts.backup && backup_root.exists() {
        return Err(Error::Other(format!(
            "{} exists: a previous patch is still applied. Revert it first, or apply with --no-backup.",
            backup_root.display()
        )));
    }
    let base = match verify_base(idx, game, opts.progress)? {
        Ok(_) if patched_over_base(idx, game)? => return Err(Error::AlreadyApplied),
        Ok(b) => b,
        Err(_) if is_patched(idx, game)? => return Err(Error::AlreadyApplied),
        Err(list) => return Err(Error::BaseMismatch(list)),
    };

    let staging = game.join(STAGING_DIR);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).at(&staging)?;
    let staged = stage(patch, &base.maps, &staging, opts.progress);
    // Windows cannot rename or delete files that are still mapped.
    let BaseFiles { paths: base_paths, maps } = base;
    drop(maps);
    let result = staged.and_then(|()| commit(patch, game, &base_paths, &staging, opts));
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Applies to a fresh copy of `game` in `out`, leaving `game` untouched.
pub fn apply_to_copy(patch: &Patch, game: &Path, out: &Path, progress: &dyn Progress) -> Result<ApplyReport> {
    if out.exists() && fs::read_dir(out).at(out)?.next().is_some() {
        return Err(Error::Other(format!("{} is not empty", out.display())));
    }
    if canonical_lenient(out)?.starts_with(fs::canonicalize(game).at(game)?) {
        return Err(Error::Other(format!("the output folder {} is inside the game folder", out.display())));
    }
    if let Err(list) = verify_base(&patch.index, game, progress)? {
        return Err(Error::BaseMismatch(list));
    }
    progress.note(&format!("Copying game to {}", out.display()));
    copy_tree(game, out)?;
    let r = apply(patch, out, &ApplyOptions { backup: false, progress });
    if r.is_err() {
        let _ = fs::remove_dir_all(out);
    }
    r
}

/// Restores the game from `.patcher-backup/`.
pub fn revert(game: &Path, progress: &dyn Progress) -> Result<(String, usize)> {
    let root = game.join(BACKUP_DIR);
    let man_path = root.join(MANIFEST);
    let raw = match fs::read(&man_path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Other(format!(
                "no backup found in {}; nothing to revert",
                game.display()
            )));
        }
        Err(e) => return Err(e).at(&man_path),
    };
    let man: Manifest =
        bitcode::decode(&raw).map_err(|e| Error::Corrupt(format!("backup manifest: {e}")))?;
    for p in man.added.iter().chain(&man.replaced).chain(&man.dirs_created).chain(&man.dirs_removed) {
        validate_rel_path(p)?;
    }
    progress.begin("Reverting", (man.added.len() + man.replaced.len()) as u64);
    for a in &man.added {
        let p = paths::join(game, a);
        match fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).at(p),
            _ => progress.inc(1),
        }
    }
    // Directories first: a created directory may stand where an original file goes back.
    for d in man.dirs_created.iter().rev() {
        let _ = fs::remove_dir(paths::join(game, d));
    }
    for d in &man.dirs_removed {
        let p = paths::join(game, d);
        fs::create_dir_all(&p).at(p)?;
    }
    let files = root.join("files");
    for r in &man.replaced {
        let src = paths::join(&files, r);
        if src.exists() {
            let dst = paths::join(game, r);
            ensure_dir(dst.parent().unwrap(), &mut Vec::new())?;
            rename(&src, &dst)?;
        }
        progress.inc(1);
    }
    fs::remove_dir_all(&root).at(&root)?;
    progress.end();
    let name = format!("{} {}", man.patch_name, man.patch_version).trim().to_string();
    Ok((name, man.added.len() + man.replaced.len()))
}

/// Resolves and checks every base file. The inner `Err` lists mismatches.
fn verify_base(idx: &Index, game: &Path, progress: &dyn Progress) -> Result<Result<BaseFiles, Vec<String>>> {
    let mut problems = Vec::new();
    let mut paths_ = Vec::with_capacity(idx.base_files.len());
    for b in &idx.base_files {
        match paths::resolve_existing(game, &b.path)? {
            Some(p) if p.is_file() => {
                if fs::metadata(&p).at(&p)?.len() != b.check.size {
                    problems.push(format!("changed: {}", b.path));
                }
                paths_.push(p);
            }
            _ => {
                problems.push(format!("missing: {}", b.path));
                paths_.push(PathBuf::new());
            }
        }
    }
    if !problems.is_empty() {
        return Ok(Err(problems));
    }
    let total = idx.base_files.iter().map(|b| b.check.size).sum();
    progress.begin("Verifying game files", total);
    let checked: Vec<Result<(Mapped, bool)>> = paths_
        .par_iter()
        .zip(&idx.base_files)
        .map(|(p, b)| {
            let m = Mapped::open(p)?;
            let ok = hash::matches(&b.check, &m);
            progress.inc(b.check.size);
            Ok((m, ok))
        })
        .collect();
    progress.end();
    let mut maps = Vec::with_capacity(checked.len());
    for (r, b) in checked.into_iter().zip(&idx.base_files) {
        let (m, ok) = r?;
        if !ok {
            problems.push(format!("changed: {}", b.path));
        }
        maps.push(m);
    }
    if problems.is_empty() { Ok(Ok(BaseFiles { paths: paths_, maps })) } else { Ok(Err(problems)) }
}

/// For a game whose base files verified: whether the outputs are in place anyway (a patch
/// that only adds files). Anything the patch overwrites or deletes would still be there.
fn patched_over_base(idx: &Index, game: &Path) -> Result<bool> {
    let base: HashMap<String, &Check> = idx.base_files.iter().map(|b| (b.path.to_lowercase(), &b.check)).collect();
    let overwrites = idx.writes.iter().any(|w| base.get(&w.path.to_lowercase()).is_some_and(|c| **c != w.check));
    if idx.writes.is_empty() || !idx.deletes.is_empty() || overwrites {
        return Ok(false);
    }
    is_patched(idx, game)
}

fn is_patched(idx: &Index, game: &Path) -> Result<bool> {
    for w in &idx.writes {
        let Some(p) = paths::resolve_existing(game, &w.path)? else { return Ok(false) };
        if !p.is_file() || fs::metadata(&p).at(&p)?.len() != w.check.size {
            return Ok(false);
        }
        if w.exec && mode_of(&p).is_some_and(|m| m & 0o111 == 0) {
            return Ok(false);
        }
    }
    for &d in &idx.deletes {
        if paths::resolve_existing(game, &idx.base_files[d as usize].path)?.is_some() {
            return Ok(false);
        }
    }
    let all_match = idx.writes.par_iter().map(|w| -> Result<bool> {
        let p = paths::resolve_existing(game, &w.path)?.unwrap();
        Ok(hash::matches(&w.check, &Mapped::open(&p)?))
    });
    all_match.try_reduce(|| true, |a, b| Ok(a && b))
}

/// Decodes every output into `staging/<write index>`.
fn stage(patch: &Patch, maps: &[Mapped], staging: &Path, progress: &dyn Progress) -> Result<()> {
    let idx = &patch.index;
    let total = idx.writes.iter().map(|w| w.check.size).sum();
    progress.begin("Patching", total);
    let budget = Budget::new((sys::total_memory() / 2).max(512 << 20));

    enum Job {
        Single(usize),
        Group(usize, Vec<usize>),
    }
    let mut members = vec![Vec::new(); idx.solid.len()];
    let mut jobs = Vec::new();
    for (i, w) in idx.writes.iter().enumerate() {
        match w.source {
            Source::Solid { group, .. } => members[group as usize].push(i),
            _ => jobs.push(Job::Single(i)),
        }
    }
    jobs.extend(members.into_iter().enumerate().map(|(g, m)| Job::Group(g, m)));
    // Largest first keeps all cores busy until the end.
    jobs.sort_by_cached_key(|j| {
        Reverse(match j {
            Job::Single(i) => idx.writes[*i].check.size,
            Job::Group(g, _) => idx.solid[*g].len,
        })
    });

    jobs.par_iter().try_for_each(|job| match job {
        Job::Single(i) => stage_single(patch, maps, staging, *i, &budget, progress),
        Job::Group(g, members) => {
            let group = &idx.solid[*g];
            let _permit = budget.acquire(group.len.saturating_mul(2));
            let data = codec::decompress(group.comp, patch.blob(group.blob)?, None, group.len as usize)?;
            members.par_iter().try_for_each(|&i| {
                let w = &idx.writes[i];
                let Source::Solid { offset, .. } = w.source else { unreachable!() };
                let slice = &data[offset as usize..(offset + w.check.size) as usize];
                let path = staging.join(i.to_string());
                fs::write(&path, slice).at(&path)?;
                progress.inc(w.check.size);
                verify_output(&w.path, &w.check, hash::check_of(slice, w.check.blake3.is_some()))
            })
        }
    })?;
    progress.end();
    Ok(())
}

fn stage_single(
    patch: &Patch,
    maps: &[Mapped],
    staging: &Path,
    i: usize,
    budget: &Budget,
    progress: &dyn Progress,
) -> Result<()> {
    let w = &patch.index.writes[i];
    let path = staging.join(i.to_string());
    let mut out = BufWriter::with_capacity(1 << 20, File::create(&path).at(&path)?);
    let mut hasher = hash::Hasher::new(w.check.blake3.is_some());
    match &w.source {
        Source::Base(b) => {
            let data = &maps[*b as usize][..];
            hasher.update(data);
            out.write_all(data).at(&path)?;
            progress.inc(data.len() as u64);
        }
        Source::Segments(segs) => {
            // Chunked (large) files decode their chunks in parallel batches, written in order.
            // One permit covers a whole batch, taken and released on this thread.
            let batch = if segs.len() > 1 { rayon::current_num_threads() } else { 1 };
            for group in segs.chunks(batch) {
                let est = group.iter().fold(0u64, |a, seg| {
                    let dict = match seg.codec {
                        Codec::Full(Comp::Lzma2 { dict_size })
                        | Codec::Ref(Comp::Lzma2 { dict_size })
                        | Codec::Bsdiff(Comp::Lzma2 { dict_size }) => u64::from(dict_size),
                        _ => 0,
                    };
                    let base = seg.base.map_or(0, |b| b.len.saturating_mul(2));
                    a.saturating_add(seg.len.saturating_mul(2)).saturating_add(base).saturating_add(dict)
                });
                let permit = budget.acquire(est);
                let decoded: Vec<Result<Vec<u8>>> = group
                    .par_iter()
                    .map(|seg| {
                        let base = seg.base.map(|bs| &maps[bs.file as usize][bs.offset as usize..(bs.offset + bs.len) as usize]);
                        codec::decode_segment(seg, patch.blob(seg.blob)?, base)
                    })
                    .collect();
                for d in decoded {
                    let data = d?;
                    hasher.update(&data);
                    out.write_all(&data).at(&path)?;
                    progress.inc(data.len() as u64);
                }
                drop(permit);
            }
        }
        Source::Solid { .. } => unreachable!("solid members are staged per group"),
    }
    out.into_inner().map_err(|e| e.into_error()).at(&path)?;
    verify_output(&w.path, &w.check, hasher.finish())
}

fn verify_output(path: &str, expected: &Check, got: Check) -> Result<()> {
    if *expected == got {
        Ok(())
    } else {
        Err(Error::Corrupt(format!("output verification failed for {path}")))
    }
}

enum Undo {
    /// Rename `.1` back to `.0`.
    Moved(PathBuf, PathBuf),
    /// Remove a file placed by the patch.
    Placed(PathBuf),
    /// Remove a directory the commit created.
    CreatedDir(PathBuf),
    /// Recreate a directory the commit removed.
    RemovedDir(PathBuf),
}

/// Comparable form of a resolved path (case-insensitive on Windows).
fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) { s.to_lowercase() } else { s.into_owned() }
}

/// Commit order: (1) originals of overwritten and deleted files leave, (2) vanished
/// directories go, (3) staged files move in, (4) new empty directories appear. This lets
/// a file replace a directory and the other way round.
fn commit(patch: &Patch, game: &Path, base_paths: &[PathBuf], staging: &Path, opts: &ApplyOptions) -> Result<ApplyReport> {
    let idx = &patch.index;
    let backup_root = game.join(BACKUP_DIR);
    let backup_files = backup_root.join("files");

    // Where each output goes and whether it displaces an original (only regular files do:
    // a directory in the way is removed through `dirs_remove`). A base file's spelling wins
    // over the write's, so case-only renames keep the on-disk name and revert restores it.
    let base_by_key: HashMap<String, &PathBuf> = base_paths.iter().map(|p| (path_key(p), p)).collect();
    let mut targets = Vec::with_capacity(idx.writes.len());
    for w in &idx.writes {
        targets.push(match paths::resolve_existing(game, &w.path)? {
            Some(p) => {
                let existed = p.is_file();
                (base_by_key.get(&path_key(&p)).map_or(p, |b| (*b).clone()), existed)
            }
            None => (paths::resolve_for_write(game, &w.path)?, false),
        });
    }
    // A delete that resolves to a write target (a case-only rename) is just an overwrite.
    let target_keys: HashSet<String> = targets.iter().map(|t| path_key(&t.0)).collect();
    let deleted: Vec<&PathBuf> = idx
        .deletes
        .iter()
        .map(|&d| &base_paths[d as usize])
        .filter(|p| !target_keys.contains(&path_key(p)))
        .collect();
    let mut seen = HashSet::new();
    let originals: Vec<&PathBuf> = targets
        .iter()
        .filter(|t| t.1)
        .map(|t| &t.0)
        .chain(deleted.iter().copied())
        .filter(|p| seen.insert(path_key(p)))
        .collect();
    // Read before the originals move away.
    let modes: Vec<Option<u32>> = targets.iter().map(|(p, existed)| if *existed { mode_of(p) } else { None }).collect();
    let mut man = Manifest {
        patch_name: idx.meta.name.clone(),
        patch_version: idx.meta.version.clone(),
        replaced: originals.iter().map(|p| paths::to_rel(game, p)).collect(),
        added: targets.iter().filter(|t| !t.1).map(|t| paths::to_rel(game, &t.0)).collect(),
        ..Default::default()
    };
    if opts.backup {
        // Written first so a crash mid-commit can still be reverted.
        write_manifest(&backup_root, &man)?;
    }

    let mut undo = Vec::new();
    let mut created = Vec::new();
    let result = (|| -> Result<()> {
        if opts.backup {
            for (orig, rel) in originals.iter().zip(&man.replaced) {
                let dst = paths::join(&backup_files, rel);
                ensure_dir(dst.parent().unwrap(), &mut Vec::new())?;
                rename(orig, &dst)?;
                undo.push(Undo::Moved((*orig).clone(), dst));
            }
        } else {
            for p in &deleted {
                match fs::remove_file(p) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).at(*p),
                    _ => {}
                }
            }
        }

        let mut remove: Vec<&String> = idx.dirs_remove.iter().collect();
        remove.sort_by_key(|d| Reverse(d.matches('/').count()));
        for d in remove {
            if let Some(p) = paths::resolve_existing(game, d)?
                && p.is_dir()
                && fs::remove_dir(&p).is_ok()
            {
                man.dirs_removed.push(paths::to_rel(game, &p));
                undo.push(Undo::RemovedDir(p));
            }
        }

        // Sequential on purpose: parallel renames contend on NTFS directory locks and
        // measured 4-8x slower for hundreds of small files.
        for (path, _) in &targets {
            let n = created.len();
            ensure_dir(path.parent().unwrap(), &mut created)?;
            undo.extend(created[n..].iter().cloned().map(Undo::CreatedDir));
        }
        let placed: Vec<Result<()>> = targets
            .iter()
            .zip(&idx.writes)
            .zip(&modes)
            .enumerate()
            .map(|(i, (((path, _), w), mode))| {
                let staged = staging.join(i.to_string());
                set_mode(&staged, w.exec, *mode)?;
                rename(&staged, path)
            })
            .collect();
        let mut first_err = None;
        for (r, (path, existed)) in placed.into_iter().zip(&targets) {
            match r {
                Ok(()) if !existed || opts.backup => undo.push(Undo::Placed(path.clone())),
                Ok(()) => {}
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }

        for d in &idx.dirs_create {
            let n = created.len();
            ensure_dir(&paths::resolve_for_write(game, d)?, &mut created)?;
            undo.extend(created[n..].iter().cloned().map(Undo::CreatedDir));
        }
        Ok(())
    })();

    if let Err(e) = result {
        let mut clean = true;
        for u in undo.into_iter().rev() {
            clean &= match u {
                Undo::Placed(p) => fs::remove_file(p).is_ok(),
                Undo::CreatedDir(d) => fs::remove_dir(d).is_ok(),
                Undo::RemovedDir(d) => fs::create_dir_all(d).is_ok(),
                Undo::Moved(orig, dst) => rename(&dst, &orig).is_ok(),
            };
        }
        if !clean {
            let note = if opts.backup {
                format!("the originals remain in {}", backup_files.display())
            } else {
                "the game may be partly patched".into()
            };
            return Err(Error::Other(format!("{e}\nRolling back failed; {note}.")));
        }
        if opts.backup {
            let _ = fs::remove_dir_all(&backup_root);
        }
        return Err(e);
    }

    man.dirs_created = created.iter().map(|d| paths::to_rel(game, d)).collect();
    if opts.backup {
        write_manifest(&backup_root, &man)?;
    }
    Ok(ApplyReport {
        written: idx.writes.len(),
        written_bytes: idx.writes.iter().map(|w| w.check.size).sum(),
        deleted: deleted.len(),
        backed_up: opts.backup,
    })
}

/// `fs::rename` that copes with Windows quirks: a freshly written file can be briefly
/// locked by antivirus scanners, and read-only targets refuse to be replaced.
fn rename(from: &Path, to: &Path) -> Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 5 && retryable(&e) => {
                if let Ok(m) = fs::metadata(to) {
                    let mut p = m.permissions();
                    if p.readonly() {
                        #[allow(clippy::permissions_set_readonly_false)]
                        p.set_readonly(false);
                        let _ = fs::set_permissions(to, p);
                    }
                }
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50 << attempt));
            }
            Err(e) => return Err(e).at(to),
        }
    }
}

/// Access denied, sharing violation, lock violation: usually a scanner holding the file.
fn retryable(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || (cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)))
}

fn write_manifest(root: &Path, man: &Manifest) -> Result<()> {
    fs::create_dir_all(root).at(root)?;
    let tmp = root.join("manifest.tmp");
    fs::write(&tmp, bitcode::encode(man)).at(&tmp)?;
    let path = root.join(MANIFEST);
    fs::rename(&tmp, &path).at(path)
}

/// Creates `dir` and missing ancestors, recording each directory it created.
fn ensure_dir(dir: &Path, created: &mut Vec<PathBuf>) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        ensure_dir(parent, created)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => created.push(dir.to_path_buf()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Error::Other(format!("{} exists and is not a directory", dir.display())));
        }
        Err(e) => return Err(e).at(dir),
    }
    Ok(())
}

#[cfg(unix)]
fn mode_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn mode_of(_: &Path) -> Option<u32> {
    None
}

/// `original`: the mode of the file this output replaces.
#[cfg(unix)]
fn set_mode(staged: &Path, exec: bool, original: Option<u32>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let x = if exec { 0o111 } else { 0 };
    fs::set_permissions(staged, fs::Permissions::from_mode(original.unwrap_or(0o644) | x)).at(staged)
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: bool, _: Option<u32>) -> Result<()> {
    Ok(())
}

/// Canonical form of a path that may not exist yet: its nearest existing ancestor is
/// canonicalized and the rest appended.
fn canonical_lenient(p: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(p).at(p)?;
    let mut tail = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(c) = fs::canonicalize(cur) {
            return Ok(tail.into_iter().rev().fold(c, |c, t| c.join(t)));
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent;
            }
            _ => return Err(Error::Other(format!("cannot resolve {}", p.display()))),
        }
    }
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).at(dst)?;
    for e in fs::read_dir(src).at(src)? {
        let e = e.at(src)?;
        let name = e.file_name();
        if name == BACKUP_DIR || name == STAGING_DIR {
            continue;
        }
        let ty = e.file_type().at(e.path())?;
        let to = dst.join(&name);
        if ty.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(e.path(), &to).at(&to)?;
        }
    }
    Ok(())
}
