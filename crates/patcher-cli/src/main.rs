//! `patcher`: the mod author's tool. Creates patches (and standalone patchers) and can
//! also apply, verify, revert and inspect them.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use patcher_core::format::Meta;
use patcher_core::progress::{human_bytes, Console};
use patcher_core::{ui, ApplyOptions, GameState, Patch};
use patcher_create::{Action, CreateOptions, CreateReport, Preset};
use serde::Deserialize;

mod stubs {
    include!(concat!(env!("OUT_DIR"), "/stubs.rs"));
}

#[derive(Parser)]
#[command(name = "patcher", version, about = "Create and apply game mod patches")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Worker threads (default: all cores).
    #[arg(short = 'j', long, global = true)]
    threads: Option<usize>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Diff a base game folder against a modded copy and write a patch + patchers.
    Create(Box<CreateArgs>),
    /// Apply a patch to a game folder.
    Apply(ApplyArgs),
    /// Check whether a game folder matches a patch (unpatched / patched / mismatch).
    Verify { patch: PathBuf, game: PathBuf },
    /// Restore the original files from the backup left by `apply`.
    Revert { game: PathBuf },
    /// Preview a GUI theme: a simulated install (or a verify-only run of --patch) that
    /// reloads whenever the theme's folder changes.
    GuiPreview(PreviewArgs),
    /// Show a patch's metadata and contents.
    Info {
        patch: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ProfileArg {
    Fast,
    Balanced,
    Max,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Target {
    Windows,
    Linux,
    All,
}

#[derive(Clone, Copy, ValueEnum, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum Ui {
    /// Installer window (default).
    Gui,
    /// Console patcher.
    Cli,
}

#[derive(clap::Args)]
struct CreateArgs {
    /// Unmodified game folder.
    #[arg(long)]
    base: PathBuf,
    /// Modded game folder.
    #[arg(long)]
    modded: PathBuf,
    /// Output folder.
    #[arg(short, long, default_value = ".")]
    out_dir: PathBuf,
    /// TOML file with name/version/author/description/url/exclude/profile.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Mod name (also used for output file names).
    #[arg(long)]
    name: Option<String>,
    /// Mod version shown to players.
    #[arg(long = "mod-version")]
    mod_version: Option<String>,
    /// Mod author.
    #[arg(long)]
    author: Option<String>,
    /// One-line description shown before applying.
    #[arg(long)]
    description: Option<String>,
    /// Homepage shown to players.
    #[arg(long)]
    url: Option<String>,
    /// Compression effort.
    #[arg(long, value_enum)]
    profile: Option<ProfileArg>,
    /// Accept a faster-to-decode encoding up to this many percent larger than the smallest.
    #[arg(long)]
    tolerance: Option<f64>,
    /// Do not store BLAKE3 hashes (smaller patch, faster apply; size + xxh3 still checked).
    #[arg(long)]
    no_verify: bool,
    /// Glob of paths to leave out (gitignore syntax), repeatable.
    #[arg(long)]
    exclude: Vec<String>,
    /// Extra gitignore-style file; <modded>/.patcherignore is read automatically.
    #[arg(long)]
    ignore_file: Vec<PathBuf>,
    /// Platforms to build standalone patchers for.
    #[arg(long, value_enum, default_value = "all")]
    target: Target,
    /// Only write the .patch file.
    #[arg(long)]
    no_standalone: bool,
    /// Patcher front end: an installer window or a console program.
    #[arg(long, value_enum)]
    ui: Option<Ui>,
    /// Theme for the installer window (implies --ui gui); see docs/gui.md.
    #[arg(long, value_name = "THEME.toml")]
    gui: Option<PathBuf>,
    /// Write the patchers without the patch inside; they read the .patch next to them.
    #[arg(long, conflicts_with = "no_standalone")]
    detached: bool,
    /// Folder with prebuilt stubs (patcher-stub[-gui]-windows.exe, patcher-stub[-gui]-linux).
    #[arg(long)]
    stub_dir: Option<PathBuf>,
    /// Show what would change without encoding anything.
    #[arg(long)]
    dry_run: bool,
    /// Memory budget for encoders, e.g. 8G or 512M (default: 75 % of RAM).
    #[arg(long, value_parser = parse_size)]
    max_mem: Option<u64>,
    /// Write a per-file Markdown report here.
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(clap::Args)]
struct PreviewArgs {
    /// Theme file (default: the built-in look).
    #[arg(long, value_name = "THEME.toml")]
    gui: Option<PathBuf>,
    /// Show this patch's details and verify it against the game folder (never applies).
    #[arg(long)]
    patch: Option<PathBuf>,
    /// Game folder to start with.
    #[arg(long)]
    game: Option<PathBuf>,
    /// End the simulated install on the error screen.
    #[arg(long)]
    fail: bool,
    /// Render every screen to PNG files in this folder and exit.
    #[arg(long, value_name = "DIR")]
    screenshot: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ApplyArgs {
    patch: PathBuf,
    game: PathBuf,
    /// Do not keep originals (revert will not be possible).
    #[arg(long)]
    no_backup: bool,
    /// Write a patched copy of the game here instead of patching in place.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Do not ask for confirmation.
    #[arg(short, long)]
    yes: bool,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    name: Option<String>,
    version: Option<String>,
    author: Option<String>,
    description: Option<String>,
    url: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
    profile: Option<ProfileArg>,
    tolerance: Option<f64>,
    ui: Option<Ui>,
    /// Theme file, relative to this config file.
    gui: Option<PathBuf>,
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mul) = match s.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&s[..s.len() - 1], 1u64 << 10),
        Some('M') => (&s[..s.len() - 1], 1 << 20),
        Some('G') => (&s[..s.len() - 1], 1 << 30),
        Some('T') => (&s[..s.len() - 1], 1 << 40),
        _ => (s, 1),
    };
    num.trim().parse::<f64>().map(|n| (n * mul as f64) as u64).map_err(|e| format!("bad size {s:?}: {e}"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(n) = cli.threads {
        rayon::ThreadPoolBuilder::new().num_threads(n).build_global().ok();
    }
    let r = match cli.cmd {
        Cmd::Create(a) => create(*a),
        Cmd::Apply(a) => apply(a),
        Cmd::Verify { patch, game } => verify(&patch, &game),
        Cmd::Revert { game } => {
            patcher_core::revert(&game, &Console::default()).map(|(name, n)| println!("Reverted {name}: {n} files restored")).map_err(Into::into)
        }
        Cmd::GuiPreview(a) => gui_preview(a),
        Cmd::Info { patch, json } => info(&patch, json),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nError: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn create(a: CreateArgs) -> Result<()> {
    let cfg: ConfigFile = match &a.config {
        Some(p) => toml::from_str(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
            .with_context(|| format!("parsing {}", p.display()))?,
        None => ConfigFile::default(),
    };
    let preset = match a.profile.or(cfg.profile).unwrap_or(ProfileArg::Balanced) {
        ProfileArg::Fast => Preset::Fast,
        ProfileArg::Balanced => Preset::Balanced,
        ProfileArg::Max => Preset::Max,
    };
    let mut profile = preset.profile();
    if let Some(t) = a.tolerance.or(cfg.tolerance) {
        profile.tolerance = t / 100.0;
    }
    let pick = |cli: Option<String>, file: Option<String>| cli.or(file).unwrap_or_default();
    let meta = Meta {
        name: pick(a.name, cfg.name),
        version: pick(a.mod_version, cfg.version),
        author: pick(a.author, cfg.author),
        description: pick(a.description, cfg.description),
        url: pick(a.url, cfg.url),
        created_by: String::new(),
    };
    let mut excludes = cfg.exclude;
    excludes.extend(a.exclude);
    let opts = CreateOptions {
        base: a.base,
        modded: a.modded,
        meta,
        profile,
        strong_hashes: !a.no_verify,
        excludes,
        ignore_files: a.ignore_file,
        max_mem: a.max_mem,
    };
    let progress = Console::default();

    if a.dry_run {
        let plan = patcher_create::plan(&opts, &progress)?;
        for w in plan.warnings() {
            eprintln!("warning: {w}");
        }
        let mut totals: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
        for (action, path, size) in plan.summary() {
            let (tag, extra) = describe(&action);
            println!("{tag:<9} {path}{extra}  ({})", human_bytes(size));
            let t = totals.entry(tag).or_default();
            t.0 += 1;
            t.1 += size;
        }
        println!();
        for (tag, (n, size)) in totals {
            println!("{tag:<9} {n} files, {}", human_bytes(size));
        }
        println!("unchanged {} files", plan.unchanged());
        return Ok(());
    }

    let config_dir = a.config.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let theme_file = a.gui.clone().or_else(|| cfg.gui.as_ref().map(|g| config_dir.join(g)));
    let ui = if theme_file.is_some() { Ui::Gui } else { a.ui.or(cfg.ui).unwrap_or(Ui::Gui) };
    if theme_file.is_some() && a.ui.or(cfg.ui) == Some(Ui::Cli) {
        bail!("--gui needs the GUI patcher; drop --ui cli");
    }
    // Checked before encoding, so a theme mistake does not cost a full patch build.
    let (theme, bundle) = if ui == Ui::Gui && !a.no_standalone {
        let (t, b) = prepare_gui(theme_file.as_deref())?;
        (Some(t), b)
    } else {
        (None, None)
    };

    std::fs::create_dir_all(&a.out_dir)?;
    let stem = file_stem(&opts.meta.name);
    let patch_path = a.out_dir.join(format!("{stem}.patch"));
    let report = patcher_create::create(&opts, &patch_path, &progress)?;
    print_summary(&report);
    println!("\nWrote {} ({})", patch_path.display(), human_bytes(report.patch_size));
    if let Some(r) = &a.report {
        std::fs::write(r, markdown_report(&report))?;
        println!("Wrote {}", r.display());
    }

    if !a.no_standalone {
        let targets: &[Target] = match a.target {
            Target::All => &[Target::Windows, Target::Linux],
            Target::Windows => &[Target::Windows],
            Target::Linux => &[Target::Linux],
        };
        let embedded_patch = (!a.detached).then_some(patch_path.as_path());
        for &t in targets {
            let out = a.out_dir.join(match t {
                Target::Windows => format!("{stem}-patcher.exe"),
                _ => format!("{stem}-patcher"),
            });
            let os = if t == Target::Windows { "Windows" } else { "Linux" };
            let mut ui = ui;
            let stub = match find_stub(t, ui, a.stub_dir.as_deref()) {
                Some(s) => Some(s),
                None if ui == Ui::Gui && t == Target::Linux => {
                    eprintln!(
                        "warning: no Linux GUI stub in this build (build the creator on Linux); the Linux patcher will be a console app"
                    );
                    ui = Ui::Cli;
                    find_stub(t, Ui::Cli, a.stub_dir.as_deref())
                }
                None => None,
            };
            let Some(stub) = stub else {
                eprintln!(
                    "warning: no {os} {} stub available; skipped {} (use --stub-dir or build with `cargo xtask dist`)",
                    if ui == Ui::Gui { "GUI" } else { "console" },
                    out.display()
                );
                continue;
            };
            let stub = if t == Target::Windows {
                let icon = theme.as_ref().and_then(|t| t.icon.clone()).filter(|p| {
                    p.extension().is_some_and(|e| ["png", "jpg", "jpeg"].iter().any(|x| e.eq_ignore_ascii_case(x)))
                });
                match patcher_create::pe::brand_windows_stub(&stub, icon.as_deref(), &opts.meta) {
                    Ok(b) => Cow::Owned(b),
                    Err(e) => {
                        eprintln!("warning: could not set the Windows icon and version info: {e:#}");
                        stub
                    }
                }
            } else {
                stub
            };
            let bundle = if ui == Ui::Gui { bundle.as_deref() } else { None };
            patcher_create::write_standalone(&stub, bundle, embedded_patch, &out)?;
            println!("Wrote {} ({})", out.display(), human_bytes(std::fs::metadata(&out)?.len()));
        }
    }
    Ok(())
}

/// Loads, compiles and packs the theme for GUI patchers, so a broken theme fails here and
/// never reaches players. Returns the theme (for the icon) and the bundle (if a theme file
/// was given; without one the patcher uses the built-in look).
fn prepare_gui(theme_file: Option<&Path>) -> Result<(patcher_gui::Theme, Option<Vec<u8>>)> {
    let theme = match theme_file {
        Some(p) => patcher_gui::Theme::load(p)?,
        None => patcher_gui::Theme::builtin(),
    };
    let work = std::env::temp_dir().join(format!("patcher-theme-check-{}", std::process::id()));
    let checked = patcher_gui::compile::compile(&theme, &work);
    let _ = std::fs::remove_dir_all(&work);
    checked.map_err(|e| anyhow::anyhow!("{e}"))?;
    let bundle = theme_file.map(patcher_gui::bundle::pack).transpose()?;
    Ok((theme, bundle))
}

fn gui_preview(a: PreviewArgs) -> Result<()> {
    use patcher_gui::app::{RunOptions, StartMode};
    use patcher_gui::backend::Backend;
    let theme = match &a.gui {
        Some(p) => patcher_gui::Theme::load(p)?,
        None => patcher_gui::Theme::builtin(),
    };
    let backend: std::sync::Arc<dyn Backend> = match &a.patch {
        Some(p) => std::sync::Arc::new(patcher_gui::sim::VerifyOnly::new(Patch::open(p).with_context(|| p.display().to_string())?)),
        None => std::sync::Arc::new(patcher_gui::sim::SimBackend { meta: patcher_gui::snapshot::demo_meta(), fail: a.fail }),
    };
    let work = std::env::temp_dir().join(format!("patcher-preview-{}", std::process::id()));
    let r = if let Some(dir) = &a.screenshot {
        patcher_gui::snapshot::render_all(&theme, &backend.meta(), &work, dir).map(|files| {
            for f in files {
                println!("{}", f.display());
            }
        })
    } else {
        let start = match a.game {
            Some(g) => StartMode::Folder(g),
            None => StartMode::Detect { exe_dir: std::env::current_dir()? },
        };
        if a.gui.is_some() {
            eprintln!("Watching the theme folder; save a file to reload.");
        }
        patcher_gui::app::run(RunOptions { theme, backend, start, work: work.clone(), watch: a.gui })
    };
    let _ = std::fs::remove_dir_all(&work);
    r.map_err(|e| anyhow::anyhow!(e))
}

fn file_stem(name: &str) -> String {
    let s: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if s.is_empty() { "mod".into() } else { s }
}

fn find_stub(t: Target, ui: Ui, dir: Option<&Path>) -> Option<Cow<'static, [u8]>> {
    let (embedded, names): (_, &[&str]) = match (t, ui) {
        (Target::Windows, Ui::Cli) => (stubs::STUB_WINDOWS, &["patcher-stub-windows.exe"]),
        (Target::Windows, Ui::Gui) => (stubs::STUB_WINDOWS_GUI, &["patcher-stub-gui-windows.exe"]),
        (_, Ui::Cli) => (stubs::STUB_LINUX, &["patcher-stub-linux"]),
        (_, Ui::Gui) => (stubs::STUB_LINUX_GUI, &["patcher-stub-gui-linux"]),
    };
    if let Some(b) = embedded {
        return Some(Cow::Borrowed(b));
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let mut cands: Vec<PathBuf> = Vec::new();
    for d in dir.into_iter().map(Path::to_path_buf).chain([exe_dir.clone(), exe_dir.join("stubs")]) {
        cands.extend(names.iter().map(|n| d.join(n)));
    }
    // Development builds: stubs built into the same cargo target directory.
    let profile = exe_dir.file_name().map(|p| p.to_os_string()).unwrap_or_default();
    let bin = if ui == Ui::Gui { "patcher-stub-gui" } else { "patcher-stub" };
    match t {
        Target::Windows if cfg!(windows) => cands.push(exe_dir.join(format!("{bin}.exe"))),
        Target::Windows => cands.push(exe_dir.join("../x86_64-pc-windows-gnu").join(&profile).join(format!("{bin}.exe"))),
        _ if ui == Ui::Gui && cfg!(target_os = "linux") => cands.push(exe_dir.join(bin)),
        _ if ui == Ui::Gui => {}
        _ => cands.push(exe_dir.join("../x86_64-unknown-linux-musl").join(&profile).join(bin)),
    }
    cands.into_iter().find_map(|p| std::fs::read(p).ok()).map(Cow::Owned)
}

fn describe(a: &Action) -> (&'static str, String) {
    match a {
        Action::Modified => ("modified", String::new()),
        Action::Renamed { from } => ("renamed", format!(" (from {from})")),
        Action::Copied { from } => ("copied", format!(" (= {from})")),
        Action::Added => ("added", String::new()),
        Action::Solid => ("added", String::new()),
        Action::Deleted => ("deleted", String::new()),
    }
}

fn print_summary(r: &CreateReport) {
    let mut by: BTreeMap<&str, (usize, u64, u64)> = BTreeMap::new();
    let mut methods: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for f in &r.files {
        let tag = match f.action {
            Action::Solid => "added (solid)",
            ref a => describe(a).0,
        };
        let e = by.entry(tag).or_default();
        e.0 += 1;
        e.1 += f.size;
        e.2 += f.stored;
        if !matches!(f.action, Action::Solid | Action::Deleted) {
            let m = methods.entry(f.method.as_str()).or_default();
            m.0 += 1;
            m.1 += f.stored;
        }
    }
    for g in &r.groups {
        by.get_mut("added (solid)").unwrap().2 += g.stored;
        let m = methods.entry(g.method.as_str()).or_default();
        m.0 += 1;
        m.1 += g.stored;
    }
    println!(
        "\nBase {} | modded {} | {} files unchanged",
        human_bytes(r.base_size),
        human_bytes(r.modded_size),
        r.unchanged
    );
    for (tag, (n, size, stored)) in &by {
        println!("  {tag:<14} {n:>6} files  {:>12} -> {:>12}", human_bytes(*size), human_bytes(*stored));
    }
    println!("Encodings:");
    for (m, (n, stored)) in &methods {
        println!("  {m:<18} x{n:<5} {}", human_bytes(*stored));
    }
    println!("Index {} | patch {} | {:.1}s", human_bytes(r.index_size), human_bytes(r.patch_size), r.elapsed.as_secs_f64());
}

fn markdown_report(r: &CreateReport) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "| path | action | size | stored | method | candidates (smallest first) |");
    let _ = writeln!(s, "|---|---|---:|---:|---|---|");
    let mut files: Vec<_> = r.files.iter().collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    for f in files {
        let tried: Vec<String> = f.tried.iter().take(4).map(|(n, sz)| format!("{n} {}", human_bytes(*sz))).collect();
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} |",
            f.path,
            describe(&f.action).0,
            human_bytes(f.size),
            human_bytes(f.stored),
            f.method,
            tried.join(", ")
        );
    }
    for (i, g) in r.groups.iter().enumerate() {
        let tried: Vec<String> = g.tried.iter().map(|(n, sz)| format!("{n} {}", human_bytes(*sz))).collect();
        let _ = writeln!(
            s,
            "| solid group #{i} ({} files) | added | {} | {} | {} | {} |",
            g.files,
            human_bytes(g.size),
            human_bytes(g.stored),
            g.method,
            tried.join(", ")
        );
    }
    s
}

fn apply(a: ApplyArgs) -> Result<()> {
    let patch = Patch::open(&a.patch)?;
    println!("{}", ui::banner(&patch));
    if !a.yes && !ui::confirm("Apply the patch?") {
        bail!("cancelled");
    }
    let progress = Console::default();
    let report = match &a.out {
        Some(out) => patcher_core::apply_to_copy(&patch, &a.game, out, &progress)?,
        None => patcher_core::apply(&patch, &a.game, &ApplyOptions { backup: !a.no_backup, progress: &progress, cancel: None })?,
    };
    println!(
        "Done: {} files written ({}), {} deleted{}",
        report.written,
        human_bytes(report.written_bytes),
        report.deleted,
        if report.backed_up { "; originals backed up" } else { "" }
    );
    Ok(())
}

fn verify(patch: &Path, game: &Path) -> Result<()> {
    let patch = Patch::open(patch)?;
    match patcher_core::inspect(&patch, game, &Console::default())? {
        GameState::Unpatched => println!("unpatched: the patch can be applied"),
        GameState::Patched => println!("patched: the patch is already applied"),
        GameState::Mismatch(list) => bail!("mismatch:\n  {}", list.join("\n  ")),
    }
    Ok(())
}

fn info(path: &Path, json: bool) -> Result<()> {
    let patch = Patch::open(path)?;
    let idx = &patch.index;
    if json {
        let base = |i: u32| idx.base_files[i as usize].path.clone();
        let writes: Vec<_> = idx
            .writes
            .iter()
            .map(|w| {
                let source = match &w.source {
                    patcher_core::format::Source::Base(b) => serde_json::json!({ "copy": base(*b) }),
                    patcher_core::format::Source::Solid { group, offset } => serde_json::json!({ "solid": group, "offset": offset }),
                    patcher_core::format::Source::Segments(s) => serde_json::Value::Array(
                        s.iter()
                            .map(|s| {
                                serde_json::json!({
                                    "len": s.len,
                                    "stored": s.blob.len,
                                    "codec": patcher_create::encode::label(s.codec, s.transform),
                                    "base": s.base.map(|b| serde_json::json!({ "file": base(b.file), "offset": b.offset, "len": b.len })),
                                })
                            })
                            .collect(),
                    ),
                };
                serde_json::json!({ "path": w.path, "size": w.check.size, "exec": w.exec, "source": source })
            })
            .collect();
        let m = &idx.meta;
        let v = serde_json::json!({
            "meta": { "name": m.name, "version": m.version, "author": m.author, "description": m.description, "url": m.url, "created_by": m.created_by },
            "patch_size": patch.header.total_len,
            "strong_hashes": idx.strong_hashes,
            "base_fingerprint": idx.base_fingerprint.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "base_files": idx.base_files.iter().map(|b| serde_json::json!({ "path": b.path, "size": b.check.size })).collect::<Vec<_>>(),
            "writes": writes,
            "deletes": idx.deletes.iter().map(|&d| base(d)).collect::<Vec<_>>(),
            "solid_groups": idx.solid.iter().map(|g| serde_json::json!({ "len": g.len, "stored": g.blob.len })).collect::<Vec<_>>(),
            "dirs_create": idx.dirs_create,
            "dirs_remove": idx.dirs_remove,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    print!("{}", ui::banner(&patch));
    println!("Created by {}", idx.meta.created_by);
    for w in &idx.writes {
        let how = match &w.source {
            patcher_core::format::Source::Base(b) => format!("copy of {}", idx.base_files[*b as usize].path),
            patcher_core::format::Source::Solid { group, .. } => format!("solid group {group}"),
            patcher_core::format::Source::Segments(s) => {
                let mut labels: Vec<String> = s.iter().map(|s| patcher_create::encode::label(s.codec, s.transform)).collect();
                labels.dedup();
                format!("{} ({})", labels.join(","), human_bytes(s.iter().map(|s| s.blob.len).sum()))
            }
        };
        println!("  write  {} ({}) <- {how}", w.path, human_bytes(w.check.size));
    }
    for &d in &idx.deletes {
        println!("  delete {}", idx.base_files[d as usize].path);
    }
    Ok(())
}
