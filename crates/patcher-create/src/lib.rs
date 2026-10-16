//! Patch creation: diff a base game folder against a modded one and write a patch.

pub mod bsdiff;
pub mod encode;
pub mod scan;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use patcher_core::format::{
    BaseFile, BaseSlice, Blob, Check, Comp, Footer, Header, Index, Meta, Segment, SolidGroup, Source,
    WriteOp, FORMAT_VERSION, HEADER_LEN,
};
use patcher_core::hash::{self, Mapped};
use patcher_core::progress::{Phase, Progress};
use patcher_core::sys::{self, Budget};
use rayon::prelude::*;
use xxhash_rust::xxh3::{xxh3_128, xxh3_64};

pub use encode::{Preset, Profile};
use encode::{label, pick, Encoded};
use scan::{Filter, Tree};

/// New files up to this size are packed together into solid groups.
pub const SOLID_FILE_MAX: u64 = 4 << 20;
/// Target size of a solid group.
pub const SOLID_GROUP: u64 = 64 << 20;

pub struct CreateOptions {
    pub base: PathBuf,
    pub modded: PathBuf,
    pub meta: Meta,
    pub profile: Profile,
    /// Store BLAKE3 hashes so apply verifies every file cryptographically.
    pub strong_hashes: bool,
    pub excludes: Vec<String>,
    /// Extra gitignore-style files; `<modded>/.patcherignore` is always read if present.
    pub ignore_files: Vec<PathBuf>,
    /// Memory budget for concurrent encoders (default: 75 % of RAM).
    pub max_mem: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Modified,
    /// New path, encoded against a similar base file (usually a rename).
    Renamed { from: String },
    /// New path whose content equals an existing base file.
    Copied { from: String },
    Added,
    /// Added, packed into a solid group.
    Solid,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct FileReport {
    pub path: String,
    pub action: Action,
    pub size: u64,
    /// Bytes this file costs in the patch (0 for copies and deletes, shared for solid).
    pub stored: u64,
    pub method: String,
    pub tried: Vec<(String, u64)>,
    pub time: Duration,
}

#[derive(Debug, Clone)]
pub struct GroupReport {
    pub files: usize,
    pub size: u64,
    pub stored: u64,
    pub method: String,
    pub tried: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateReport {
    pub files: Vec<FileReport>,
    pub groups: Vec<GroupReport>,
    pub unchanged: usize,
    pub base_size: u64,
    pub modded_size: u64,
    pub patch_size: u64,
    pub index_size: u64,
    pub elapsed: Duration,
}

/// What happens to one modded file.
#[derive(Debug, Clone)]
enum Kind {
    Copy { from: u32 },
    Encode { base: Option<u32> },
    Solid,
}

#[derive(Debug, Clone)]
struct Item {
    path: String,
    kind: Kind,
    action: Action,
}

/// The classified difference between two trees.
pub struct Plan {
    base: Tree,
    modded: Tree,
    items: Vec<Item>,
    /// Touched base files: (path, index in `base_files`).
    base_paths: Vec<String>,
    deletes: Vec<u32>,
    dirs_create: Vec<String>,
    dirs_remove: Vec<String>,
    unchanged: usize,
    fingerprint: [u8; 32],
}

impl Plan {
    /// Dry-run summary: (action, path, size) for every change.
    pub fn summary(&self) -> Vec<(Action, String, u64)> {
        let mut v: Vec<_> = self
            .items
            .iter()
            .map(|i| (i.action.clone(), i.path.clone(), self.modded.files[&i.path].size))
            .collect();
        v.extend(self.deletes.iter().map(|&d| {
            let p = &self.base_paths[d as usize];
            (Action::Deleted, p.clone(), self.base.files[p].size)
        }));
        v
    }

    pub fn unchanged(&self) -> usize {
        self.unchanged
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut w: Vec<String> = self.base.skipped_symlinks.iter().map(|s| format!("skipped symlink in base: {s}")).collect();
        w.extend(self.modded.skipped_symlinks.iter().map(|s| format!("skipped symlink in modded: {s}")));
        w
    }
}

fn identity(path: &Path) -> Result<(u64, u128)> {
    let m = Mapped::open(path)?;
    Ok((m.len() as u64, xxh3_128(&m)))
}

/// Scans both folders and decides what to do with every file.
pub fn plan(opts: &CreateOptions, progress: &dyn Progress) -> Result<Plan> {
    let mut ignore_files = opts.ignore_files.clone();
    let auto = opts.modded.join(scan::IGNORE_FILE);
    if auto.is_file() {
        ignore_files.push(auto);
    }
    let filter = Filter::new(&opts.excludes, &ignore_files)?;
    let base = scan::scan(&opts.base, &filter).context("scanning base folder")?;
    let modded = scan::scan(&opts.modded, &filter).context("scanning modded folder")?;

    let total: u64 = base.files.values().chain(modded.files.values()).map(|f| f.size).sum();
    progress.begin(Phase::Hash, total);
    let hash_all = |t: &Tree| -> Result<HashMap<String, (u64, u128)>> {
        t.files
            .par_iter()
            .map(|(p, f)| {
                let id = identity(&f.abs)?;
                progress.inc(f.size);
                Ok((p.clone(), id))
            })
            .collect()
    };
    let base_ids = hash_all(&base)?;
    let mod_ids = hash_all(&modded)?;
    progress.end();

    let fingerprint = {
        let mut h = blake3::Hasher::new();
        for (p, f) in &base.files {
            h.update(p.as_bytes());
            h.update(&[0]);
            h.update(&f.size.to_le_bytes());
            h.update(&base_ids[p].1.to_le_bytes());
        }
        *h.finalize().as_bytes()
    };

    let mut by_content: HashMap<(u64, u128), &str> = HashMap::new();
    for (p, id) in base.files.keys().map(|p| (p, base_ids[p])) {
        by_content.entry(id).or_insert(p);
    }
    // Paths are compared ignoring case: Windows (and apply's lookup elsewhere) resolves
    // `data/x.png` to an existing `Data/X.png`, so that is the same file, not an add + delete.
    let mut base_ci: HashMap<String, &str> = HashMap::new();
    for p in base.files.keys() {
        base_ci.entry(p.to_lowercase()).or_insert(p);
    }
    let mut mod_ci: HashMap<String, usize> = HashMap::new();
    for p in modded.files.keys() {
        *mod_ci.entry(p.to_lowercase()).or_default() += 1;
    }
    let deleted: BTreeSet<&str> =
        base.files.keys().filter(|p| !mod_ci.contains_key(&p.to_lowercase())).map(|s| s.as_str()).collect();
    let mut by_name: HashMap<String, Vec<&str>> = HashMap::new();
    for p in base.files.keys() {
        by_name.entry(file_name(p).to_lowercase()).or_default().push(p);
    }

    let mut base_paths: Vec<String> = Vec::new();
    let mut base_index: HashMap<String, u32> = HashMap::new();
    let mut reg = |p: &str| -> u32 {
        *base_index.entry(p.to_string()).or_insert_with(|| {
            base_paths.push(p.to_string());
            (base_paths.len() - 1) as u32
        })
    };

    let mut items = Vec::new();
    let mut unchanged = 0;
    for (path, f) in &modded.files {
        let id = mod_ids[path];
        if mod_ci[&path.to_lowercase()] > 1 && !base_ids.get(path).is_some_and(|b| *b == id) {
            bail!("the modded folder has several files named {path:?} ignoring case; Windows cannot hold them");
        }
        let same_path = match base.files.get_key_value(path) {
            Some((p, _)) => Some(p.as_str()),
            None => base_ci.get(&path.to_lowercase()).copied().filter(|b| !modded.files.contains_key(*b)),
        };
        if let Some(b) = same_path
            && base_ids[b] == id
        {
            if f.exec && !base.files[b].exec {
                // Only the executable bit changed (Unix): copy the file onto itself to ship it.
                items.push(Item { path: path.clone(), kind: Kind::Copy { from: reg(b) }, action: Action::Modified });
            } else {
                unchanged += 1;
            }
            continue;
        }
        if let Some(b) = same_path {
            // Overwritten in place: its base must match before we replace it.
            reg(b);
        }
        let item = if let Some(from) = by_content.get(&id).copied() {
            Item { path: path.clone(), kind: Kind::Copy { from: reg(from) }, action: Action::Copied { from: from.to_string() } }
        } else if let Some(b) = same_path {
            Item { path: path.clone(), kind: Kind::Encode { base: Some(reg(b)) }, action: Action::Modified }
        } else if let Some(from) = similar_base(&by_name, &deleted, &base, path, f.size) {
            Item { path: path.clone(), kind: Kind::Encode { base: Some(reg(from)) }, action: Action::Renamed { from: from.to_string() } }
        } else if f.size <= SOLID_FILE_MAX {
            Item { path: path.clone(), kind: Kind::Solid, action: Action::Solid }
        } else {
            Item { path: path.clone(), kind: Kind::Encode { base: None }, action: Action::Added }
        };
        items.push(item);
    }
    let deletes: Vec<u32> = deleted.iter().map(|p| reg(p)).collect();

    // Directories: create empty new ones, remove ones that vanished (deepest first at apply).
    let has_files_under = |t: &Tree, d: &str| {
        let prefix = format!("{d}/");
        t.files.range(prefix.clone()..).next().is_some_and(|(p, _)| p.starts_with(&prefix))
    };
    let has_dirs_under = |t: &Tree, d: &str| {
        let prefix = format!("{d}/");
        t.dirs.range(prefix.clone()..).next().is_some_and(|p| p.starts_with(&prefix))
    };
    let lower_set = |t: &Tree| -> HashSet<String> { t.dirs.iter().map(|d| d.to_lowercase()).collect() };
    let (base_dirs_ci, mod_dirs_ci) = (lower_set(&base), lower_set(&modded));
    let dirs_create = modded
        .dirs
        .iter()
        .filter(|d| !base_dirs_ci.contains(&d.to_lowercase()))
        .filter(|d| !has_files_under(&modded, d) && !has_dirs_under(&modded, d))
        .cloned()
        .collect();
    let dirs_remove = base.dirs.iter().filter(|d| !mod_dirs_ci.contains(&d.to_lowercase())).cloned().collect();

    Ok(Plan { base, modded, items, base_paths, deletes, dirs_create, dirs_remove, unchanged, fingerprint })
}

fn file_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// A base file that probably is the ancestor of a new file: a deleted base file with the
/// same file name and a size within 4x (a move plus edits).
fn similar_base<'a>(
    by_name: &HashMap<String, Vec<&'a str>>,
    deleted: &BTreeSet<&str>,
    base: &Tree,
    path: &str,
    size: u64,
) -> Option<&'a str> {
    let cands = by_name.get(&file_name(path).to_lowercase())?;
    cands
        .iter()
        .copied()
        .filter(|c| {
            let s = base.files[*c].size;
            deleted.contains(c) && s > 0 && size > 0 && s <= size * 4 && size <= s * 4
        })
        .min_by_key(|c| base.files[*c].size.abs_diff(size))
}

struct PatchWriter {
    out: BufWriter<File>,
    pos: u64,
}

impl PatchWriter {
    fn create(path: &Path) -> Result<Self> {
        let mut out = BufWriter::with_capacity(4 << 20, File::create(path).with_context(|| format!("creating {}", path.display()))?);
        out.write_all(&[0; HEADER_LEN])?;
        Ok(PatchWriter { out, pos: HEADER_LEN as u64 })
    }

    fn blob(&mut self, data: &[u8]) -> Result<Blob> {
        self.out.write_all(data)?;
        let b = Blob { offset: self.pos, len: data.len() as u64 };
        self.pos += data.len() as u64;
        Ok(b)
    }

    /// Writes the index and header; returns (patch size, compressed index size).
    fn finish(mut self, index: &Index) -> Result<(u64, u64)> {
        let raw = index.encode();
        let comp = encode::zstd(&raw, None, 19);
        let index_offset = self.pos;
        self.out.write_all(&comp)?;
        let total_len = index_offset + comp.len() as u64;
        let header = Header {
            version: FORMAT_VERSION,
            flags: 0,
            index_offset,
            index_len: comp.len() as u64,
            index_raw_len: raw.len() as u64,
            total_len,
        };
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&header.to_bytes())?;
        self.out.flush()?;
        Ok((total_len, comp.len() as u64))
    }
}

/// Creates the patch at `output`.
pub fn create(opts: &CreateOptions, output: &Path, progress: &dyn Progress) -> Result<CreateReport> {
    let started = Instant::now();
    let plan = plan(opts, progress)?;
    let strong = opts.strong_hashes;
    let budget = Budget::new(opts.max_mem.unwrap_or(sys::total_memory() / 4 * 3));
    let prof = &opts.profile;

    // Map and check every touched base file.
    let base_maps: Vec<Mapped> = plan
        .base_paths
        .par_iter()
        .map(|p| Mapped::open(&plan.base.files[p].abs).map_err(Into::into))
        .collect::<Result<_>>()?;
    let base_files: Vec<BaseFile> = plan
        .base_paths
        .par_iter()
        .zip(&base_maps)
        .map(|(p, m)| BaseFile { path: p.clone(), check: hash::check_of(m, strong) })
        .collect();

    let writer = Mutex::new(PatchWriter::create(output)?);
    let write_blob = |data: &[u8]| -> Result<Blob> { writer.lock().unwrap().blob(data) };

    // Encode individually stored files, largest first.
    let mut order: Vec<usize> = (0..plan.items.len()).filter(|&i| !matches!(plan.items[i].kind, Kind::Solid)).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(plan.modded.files[&plan.items[i].path].size));
    let total: u64 = plan.items.iter().map(|i| plan.modded.files[&i.path].size).sum();
    progress.begin(Phase::Encode, total);

    let encoded: Vec<(usize, WriteOp, FileReport)> = order
        .par_iter()
        .map(|&i| -> Result<_> {
            let t0 = Instant::now();
            let item = &plan.items[i];
            let f = &plan.modded.files[&item.path];
            let data = Mapped::open(&f.abs)?;
            let check = hash::check_of(&data, strong);
            let exec = f.exec || scan::looks_executable(&data);
            let (source, stored, method, tried) = match item.kind {
                Kind::Copy { from } => (Source::Base(from), 0, "copy".to_string(), vec![]),
                Kind::Encode { base } => {
                    let base_data = base.map(|b| &base_maps[b as usize][..]);
                    let segs = encode::encode_file(base_data, &data, prof, &budget);
                    let mut out = Vec::with_capacity(segs.len());
                    let (mut stored, mut methods, mut tried) = (0, BTreeSet::new(), Vec::new());
                    for s in segs {
                        let blob = write_blob(&s.chosen.data)?;
                        stored += blob.len;
                        methods.insert(label(s.chosen.codec, s.chosen.transform));
                        if tried.is_empty() || out.is_empty() {
                            tried = s.tried;
                        }
                        out.push(Segment {
                            len: s.len,
                            blob,
                            codec: s.chosen.codec,
                            transform: s.chosen.transform,
                            base: s.base.map(|(offset, len)| BaseSlice { file: base.unwrap(), offset, len }),
                        });
                    }
                    let method = methods.into_iter().collect::<Vec<_>>().join(",");
                    (Source::Segments(out), stored, if method.is_empty() { "empty".into() } else { method }, tried)
                }
                Kind::Solid => unreachable!(),
            };
            progress.inc(f.size);
            let report = FileReport {
                path: item.path.clone(),
                action: item.action.clone(),
                size: f.size,
                stored,
                method,
                tried,
                time: t0.elapsed(),
            };
            Ok((i, WriteOp { path: item.path.clone(), check, exec, source }, report))
        })
        .collect::<Result<_>>()?;

    // Solid groups: new small files sorted by type so similar data sits together.
    let mut solid: Vec<usize> = (0..plan.items.len()).filter(|&i| matches!(plan.items[i].kind, Kind::Solid)).collect();
    solid.sort_by_cached_key(|&i| {
        let p = &plan.items[i].path;
        let ext = p.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        (ext, p.clone())
    });
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut acc = u64::MAX;
    for i in solid {
        let size = plan.modded.files[&plan.items[i].path].size;
        if acc.saturating_add(size) > SOLID_GROUP {
            groups.push(Vec::new());
            acc = 0;
        }
        acc += size;
        groups.last_mut().unwrap().push(i);
    }
    type Staged = Vec<(usize, WriteOp, FileReport)>;
    let solid_out: Vec<(SolidGroup, GroupReport, Staged)> = groups
        .par_iter()
        .enumerate()
        .map(|(g, members)| -> Result<_> {
            let t0 = Instant::now();
            let mut data = Vec::new();
            let mut ops = Vec::new();
            for &i in members {
                let item = &plan.items[i];
                let f = &plan.modded.files[&item.path];
                let m = Mapped::open(&f.abs)?;
                let offset = data.len() as u64;
                data.extend_from_slice(&m);
                let op = WriteOp {
                    path: item.path.clone(),
                    check: hash::check_of(&m, strong),
                    exec: f.exec || scan::looks_executable(&m),
                    source: Source::Solid { group: g as u32, offset },
                };
                let rep = FileReport {
                    path: item.path.clone(),
                    action: Action::Solid,
                    size: f.size,
                    stored: 0,
                    method: format!("solid#{g}"),
                    tried: vec![],
                    time: Duration::ZERO,
                };
                ops.push((i, op, rep));
            }
            let (enc, tried) = encode_solid(&data, prof, &budget);
            let comp = match enc.codec {
                patcher_core::format::Codec::Full(c) => c,
                _ => unreachable!(),
            };
            let blob = write_blob(&enc.data)?;
            progress.inc(data.len() as u64);
            let report = GroupReport {
                files: members.len(),
                size: data.len() as u64,
                stored: blob.len,
                method: label(enc.codec, enc.transform),
                tried,
            };
            let _ = t0;
            Ok((SolidGroup { blob, comp, len: data.len() as u64 }, report, ops))
        })
        .collect::<Result<_>>()?;
    progress.end();

    let mut all: Vec<(usize, WriteOp, FileReport)> = encoded;
    let mut solid_groups = Vec::new();
    let mut group_reports = Vec::new();
    for (g, r, ops) in solid_out {
        solid_groups.push(g);
        group_reports.push(r);
        all.extend(ops);
    }
    all.sort_by_key(|x| x.0);

    let mut reports: Vec<FileReport> = Vec::with_capacity(all.len() + plan.deletes.len());
    let mut writes = Vec::with_capacity(all.len());
    for (_, op, rep) in all {
        writes.push(op);
        reports.push(rep);
    }
    for &d in &plan.deletes {
        let p = &plan.base_paths[d as usize];
        reports.push(FileReport {
            path: p.clone(),
            action: Action::Deleted,
            size: plan.base.files[p].size,
            stored: 0,
            method: "delete".into(),
            tried: vec![],
            time: Duration::ZERO,
        });
    }

    let mut meta = opts.meta.clone();
    meta.created_by = format!("patcher {}", env!("CARGO_PKG_VERSION"));
    let index = Index {
        meta,
        base_fingerprint: plan.fingerprint,
        strong_hashes: strong,
        base_files,
        writes,
        deletes: plan.deletes.clone(),
        solid: solid_groups,
        dirs_create: plan.dirs_create.clone(),
        dirs_remove: plan.dirs_remove.clone(),
    };
    drop(base_maps);
    let (patch_size, index_size) = writer.into_inner().unwrap().finish(&index)?;

    Ok(CreateReport {
        files: reports,
        groups: group_reports,
        unchanged: plan.unchanged,
        base_size: plan.base.files.values().map(|f| f.size).sum(),
        modded_size: plan.modded.files.values().map(|f| f.size).sum(),
        patch_size,
        index_size,
        elapsed: started.elapsed(),
    })
}

fn encode_solid(data: &[u8], prof: &Profile, budget: &Budget) -> (Encoded, Vec<(String, u64)>) {
    use patcher_core::format::{Codec, Transform};
    let n = data.len() as u64;
    let (z, l) = rayon::join(
        || {
            let _p = budget.acquire(encode::zstd_mem(data.len(), 0));
            Encoded { codec: Codec::Full(Comp::Zstd), transform: Transform::None, data: encode::zstd(data, None, prof.zstd_level) }
        },
        || {
            let preset = prof.lzma_preset.filter(|_| n <= prof.lzma_max)?;
            let d = n.clamp(4096, 1536 << 20).next_multiple_of(4096) as u32;
            let _p = budget.acquire(u64::from(d) * 12 + n * 2);
            Some(Encoded { codec: Codec::Full(Comp::Lzma2 { dict_size: d }), transform: Transform::None, data: encode::lzma(data, None, preset, d) })
        },
    );
    let mut cands = vec![z];
    cands.extend(l);
    let mut tried: Vec<_> = cands.iter().map(|c| (label(c.codec, c.transform), c.data.len() as u64)).collect();
    tried.sort_by_key(|t| t.1);
    (pick(cands, prof.tolerance), tried)
}

/// Writes `stub ++ patch ++ footer` to `out` (a self-contained patcher).
pub fn write_standalone(stub: &[u8], patch: &Path, out: &Path) -> Result<()> {
    let patch_bytes = std::fs::read(patch).with_context(|| format!("reading {}", patch.display()))?;
    let mut f = BufWriter::new(File::create(out).with_context(|| format!("creating {}", out.display()))?);
    f.write_all(stub)?;
    f.write_all(&patch_bytes)?;
    f.write_all(&Footer { patch_offset: stub.len() as u64, patch_len: patch_bytes.len() as u64 }.to_bytes())?;
    f.flush()?;
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// xxh3 of a file's contents, for reports and tests.
pub fn file_xxh3(path: &Path) -> Result<u64> {
    Ok(xxh3_64(&Mapped::open(path)?))
}

/// Exposed for reports: the check a file would get.
pub fn check_file(path: &Path, strong: bool) -> Result<Check> {
    Ok(hash::check_of(&Mapped::open(path)?, strong))
}
