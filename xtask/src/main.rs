//! Project tasks:
//!
//! ```text
//! cargo xtask dist                         build stubs for Windows + Linux, embed them in dist/patcher(.exe)
//! cargo xtask gen-corpus [--scale S] [--out DIR]
//! cargo xtask bench [--scale S] [--large GB] [--out FILE]
//! ```

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use patcher_core::format::Meta;
use patcher_core::progress::{human_bytes, Silent};
use patcher_core::{ApplyOptions, Patch};
use patcher_create::{Action, CreateOptions, CreateReport, Preset};
use patcher_testgen::{copy_tree, tree_listing};

const LINUX: &str = "x86_64-unknown-linux-musl";
const WINDOWS: &str = "x86_64-pc-windows-msvc";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let root = workspace_root();
    match args.first().map(String::as_str) {
        Some("dist") => dist(&root),
        Some("gen-corpus") => {
            let scale = flag("--scale").map_or(Ok(1.0), |s| s.parse())?;
            let out = flag("--out").map_or(root.join("target/corpus"), PathBuf::from);
            let t = Instant::now();
            let c = patcher_testgen::generate(&out, scale, 42)?;
            println!("base: {} ({:.1}s)", c.base.display(), t.elapsed().as_secs_f64());
            for s in &c.scenarios {
                println!("{:<14} {}  -- {}", s.name, s.modded.display(), s.description);
            }
            Ok(())
        }
        Some("gen-large") => {
            let gb: f64 = flag("--gb").map_or(Ok(3.0), |s| s.parse())?;
            let out = flag("--out").map_or(root.join("target/large"), PathBuf::from);
            let (b, m) = patcher_testgen::generate_large(&out, (gb * (1u64 << 30) as f64) as u64, 7)?;
            println!("{}\n{}", b.display(), m.display());
            Ok(())
        }
        Some("bench") => {
            let scale = flag("--scale").map_or(Ok(1.0), |s| s.parse())?;
            let large = flag("--large").map(|s| s.parse::<f64>()).transpose()?;
            let out = flag("--out").map_or(root.join("BENCHMARKS.md"), PathBuf::from);
            bench(&root, scale, large, &out)
        }
        _ => bail!("usage: cargo xtask <dist | gen-corpus [--scale S] [--out DIR] | bench [--scale S] [--large GB] [--out FILE]>"),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn cargo(root: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Result<()> {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root).args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    let st = c.status().context("running cargo")?;
    if !st.success() {
        bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}

/// Builds both stubs and a creator with them embedded.
fn dist(root: &Path) -> Result<()> {
    let target = root.join("target");
    let stub = |t: &str| target.join(t).join("release").join(if t == WINDOWS { "patcher-stub.exe" } else { "patcher-stub" });
    let mut envs: Vec<(&str, PathBuf)> = Vec::new();
    for (t, var) in [(WINDOWS, "PATCHER_STUB_WINDOWS"), (LINUX, "PATCHER_STUB_LINUX")] {
        match cargo(root, &["build", "--release", "-p", "patcher-stub", "--target", t], &[]) {
            Ok(()) => envs.push((var, stub(t))),
            Err(e) => eprintln!("warning: no {t} stub ({e}); install the target with `rustup target add {t}`"),
        }
    }
    let env_refs: Vec<(&str, &Path)> = envs.iter().map(|(k, v)| (*k, v.as_path())).collect();
    cargo(root, &["build", "--release", "-p", "patcher-cli"], &env_refs)?;

    let dist = root.join("dist");
    std::fs::create_dir_all(dist.join("stubs"))?;
    let exe = if cfg!(windows) { "patcher.exe" } else { "patcher" };
    std::fs::copy(target.join("release").join(exe), dist.join(exe))?;
    for (var, p) in &envs {
        let name = if *var == "PATCHER_STUB_WINDOWS" { "patcher-stub-windows.exe" } else { "patcher-stub-linux" };
        std::fs::copy(p, dist.join("stubs").join(name))?;
    }
    for e in std::fs::read_dir(&dist)?.chain(std::fs::read_dir(dist.join("stubs"))?) {
        let e = e?;
        if e.file_type()?.is_file() {
            println!("{:<40} {}", e.path().strip_prefix(root)?.display(), human_bytes(e.metadata()?.len()));
        }
    }
    Ok(())
}

struct Row {
    name: String,
    description: String,
    changed: u64,
    baseline: u64,
    report: CreateReport,
    apply_secs: f64,
}

fn opts(base: &Path, modded: &Path, preset: Preset, strong: bool) -> CreateOptions {
    CreateOptions {
        base: base.into(),
        modded: modded.into(),
        meta: Meta { name: "Bench".into(), ..Default::default() },
        profile: preset.profile(),
        strong_hashes: strong,
        excludes: vec![],
        ignore_files: vec![],
        max_mem: None,
    }
}

/// zstd -19 --long over the concatenation of every changed/added file: what a mod author
/// would get by simply shipping the changed files in a .tar.zst.
fn baseline(modded: &Path, report: &CreateReport) -> Result<(u64, u64)> {
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 19)?;
    enc.multithread(std::thread::available_parallelism()?.get() as u32)?;
    enc.long_distance_matching(true)?;
    enc.window_log(27)?;
    let mut changed = 0;
    for f in &report.files {
        if matches!(f.action, Action::Deleted | Action::Copied { .. }) {
            continue;
        }
        let d = std::fs::read(patcher_core::paths::join(modded, &f.path))?;
        changed += d.len() as u64;
        enc.write_all(&d)?;
    }
    Ok((changed, enc.finish()?.len() as u64))
}

/// Applies `patch` to a fresh copy of the base; returns seconds spent in apply.
fn timed_apply(patch_path: &Path, base: &Path, modded: &Path, game: &Path) -> Result<f64> {
    let _ = std::fs::remove_dir_all(game);
    copy_tree(base, game)?;
    let t = Instant::now();
    let patch = Patch::open(patch_path)?;
    patcher_core::apply(&patch, game, &ApplyOptions { backup: true, progress: &Silent, cancel: None })?;
    let secs = t.elapsed().as_secs_f64();
    if tree_listing(game)? != tree_listing(modded)? {
        bail!("{}: patched game differs from the modded folder", patch_path.display());
    }
    std::fs::remove_dir_all(game)?;
    Ok(secs)
}

fn mbps(bytes: u64, secs: f64) -> String {
    format!("{:.0} MB/s", bytes as f64 / 1e6 / secs.max(1e-9))
}

fn bench(root: &Path, scale: f64, large: Option<f64>, out: &Path) -> Result<()> {
    let work = root.join("target/bench");
    let t = Instant::now();
    eprintln!("generating corpus (scale {scale})...");
    let corpus = patcher_testgen::generate(&work.join("corpus"), scale, 42)?;
    let gen_secs = t.elapsed().as_secs_f64();
    let base_size: u64 = tree_listing(&corpus.base)?
        .iter()
        .filter_map(|l| l.split(' ').nth(1).and_then(|n| n.parse::<u64>().ok()))
        .sum();

    let mut rows = Vec::new();
    for sc in &corpus.scenarios {
        eprintln!("scenario {}...", sc.name);
        let patch = work.join(format!("{}.patch", sc.name));
        let report = patcher_create::create(&opts(&corpus.base, &sc.modded, Preset::Balanced, true), &patch, &Silent)?;
        let apply_secs = timed_apply(&patch, &corpus.base, &sc.modded, &work.join("game"))?;
        let (changed, baseline) = baseline(&sc.modded, &report)?;
        rows.push(Row { name: sc.name.into(), description: sc.description.into(), changed, baseline, report, apply_secs });
    }

    let full = corpus.scenarios.iter().find(|s| s.name == "full-mod").unwrap();
    let mut profiles = Vec::new();
    for (preset, strong) in [(Preset::Fast, true), (Preset::Balanced, true), (Preset::Balanced, false), (Preset::Max, true)] {
        eprintln!("profile {preset:?} strong={strong}...");
        let patch = work.join(format!("full-{preset:?}-{strong}.patch"));
        let report = patcher_create::create(&opts(&corpus.base, &full.modded, preset, strong), &patch, &Silent)?;
        let secs = timed_apply(&patch, &corpus.base, &full.modded, &work.join("game"))?;
        profiles.push((preset, strong, report, secs));
    }

    let large_row = match large {
        Some(gb) => {
            eprintln!("large file ({gb} GB)...");
            let (b, m) = patcher_testgen::generate_large(&work.join("large"), (gb * (1u64 << 30) as f64) as u64, 7)?;
            let patch = work.join("large.patch");
            let report = patcher_create::create(&opts(&b, &m, Preset::Balanced, true), &patch, &Silent)?;
            let secs = timed_apply(&patch, &b, &m, &work.join("large-game"))?;
            let _ = std::fs::remove_dir_all(work.join("large"));
            Some((gb, report, secs))
        }
        None => None,
    };

    // ---- report ----
    let mut s = String::new();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let _ = writeln!(s, "# Benchmarks\n");
    let _ = writeln!(
        s,
        "Generated by `cargo xtask bench --scale {scale}{}` on {} ({threads} threads, {} RAM). \
         The corpus is synthetic (see `crates/patcher-testgen`): a {} base game generated in {gen_secs:.1}s. \
         Apply times include verifying the touched base files, decoding, verifying outputs and committing with backups; \
         files are in the OS cache.\n",
        large.map(|g| format!(" --large {g}")).unwrap_or_default(),
        std::env::consts::OS,
        human_bytes(patcher_core::sys::total_memory()),
        human_bytes(base_size),
    );
    let _ = writeln!(s, "## Scenarios (balanced profile, BLAKE3 verification)\n");
    let _ = writeln!(s, "Baseline = `zstd -19 --long` over all changed and added files, i.e. shipping the changed files.\n");
    let _ = writeln!(s, "| scenario | changed data | baseline | patch | patch / baseline | create | apply | apply speed |");
    let _ = writeln!(s, "|---|---:|---:|---:|---:|---:|---:|---:|");
    for r in &rows {
        let written: u64 = r.report.files.iter().filter(|f| f.action != Action::Deleted).map(|f| f.size).sum();
        let _ = writeln!(
            s,
            "| {} | {} | {} | **{}** | {:.2}% | {:.1}s | {:.2}s | {} |",
            r.name,
            human_bytes(r.changed),
            human_bytes(r.baseline),
            human_bytes(r.report.patch_size),
            r.report.patch_size as f64 * 100.0 / r.baseline.max(1) as f64,
            r.report.elapsed.as_secs_f64(),
            r.apply_secs,
            mbps(written, r.apply_secs),
        );
    }
    let _ = writeln!(s);
    for r in &rows {
        let _ = writeln!(s, "- **{}**: {}", r.name, r.description);
    }

    let _ = writeln!(s, "\n## Profiles on `full-mod`\n");
    let _ = writeln!(s, "| profile | hashes | patch | create | apply | apply speed |");
    let _ = writeln!(s, "|---|---|---:|---:|---:|---:|");
    for (p, strong, r, secs) in &profiles {
        let written: u64 = r.files.iter().filter(|f| f.action != Action::Deleted).map(|f| f.size).sum();
        let _ = writeln!(
            s,
            "| {p:?} | {} | {} | {:.1}s | {:.2}s | {} |",
            if *strong { "BLAKE3" } else { "xxh3 only" },
            human_bytes(r.patch_size),
            r.elapsed.as_secs_f64(),
            secs,
            mbps(written, *secs)
        );
    }

    if let Some((gb, r, secs)) = &large_row {
        let written: u64 = r.files.iter().map(|f| f.size).sum();
        let _ = writeln!(s, "\n## Large file\n");
        let _ = writeln!(
            s,
            "One {gb} GB file; the modded copy inserts a 1 MiB block a third of the way in, rewrites 4 KiB pages in ~1% of \
             the 1 MiB blocks and appends 10 MiB. Encoded in {} MiB chunks against anchor-matched base windows.\n",
            patcher_create::encode::CHUNK >> 20
        );
        let _ = writeln!(s, "| patch | create | apply | apply speed |\n|---:|---:|---:|---:|");
        let _ = writeln!(s, "| {} | {:.1}s | {:.2}s | {} |", human_bytes(r.patch_size), r.elapsed.as_secs_f64(), secs, mbps(written, *secs));
    }

    let fm = &rows.iter().find(|r| r.name == "full-mod").unwrap().report;
    let _ = writeln!(s, "\n## What the encoder chose (`full-mod`, balanced)\n");
    let _ = writeln!(s, "| file | action | size | stored | chosen | candidates (smallest first) |");
    let _ = writeln!(s, "|---|---|---:|---:|---|---|");
    let mut files: Vec<_> = fm.files.iter().filter(|f| !matches!(f.action, Action::Solid | Action::Deleted)).collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    for f in files.iter().take(12) {
        let tried: Vec<String> = f.tried.iter().map(|(n, sz)| format!("{n} {}", human_bytes(*sz))).collect();
        let _ = writeln!(
            s,
            "| {} | {:?} | {} | {} | {} | {} |",
            f.path,
            f.action,
            human_bytes(f.size),
            human_bytes(f.stored),
            f.method,
            tried.join(", ")
        );
    }
    for (i, g) in fm.groups.iter().enumerate() {
        let tried: Vec<String> = g.tried.iter().map(|(n, sz)| format!("{n} {}", human_bytes(*sz))).collect();
        let _ = writeln!(
            s,
            "| solid group #{i} | {} new files | {} | {} | {} | {} |",
            g.files,
            human_bytes(g.size),
            human_bytes(g.stored),
            g.method,
            tried.join(", ")
        );
    }
    std::fs::write(out, &s)?;
    println!("{s}");
    eprintln!("wrote {}", out.display());
    Ok(())
}
