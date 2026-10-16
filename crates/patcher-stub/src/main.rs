//! End-user patcher: applies the patch embedded in this executable (or a `.patch` file
//! next to it) to a game folder. Kept dependency-light so it stays small.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use patcher_core::progress::{human_bytes, Console};
use patcher_core::{ui, ApplyOptions, Error, GameState, Patch};

const USAGE: &str = "\
Usage: {exe} [GAME_DIR] [options]

Applies the bundled mod patch to GAME_DIR. Without GAME_DIR the patcher looks for the
game next to itself and in the current folder, and asks if it cannot find it.

Options:
  --patch FILE    use this patch file instead of the bundled one
  -y, --yes       do not ask for confirmation
  --no-backup     do not keep the original files (revert will not be possible)
  --out DIR       write a patched copy of the game to DIR instead of patching in place
  --verify        only check whether the game matches the patch
  --revert        restore the original files from the backup
  --info          show what the patch contains
  -h, --help      show this help
";

#[derive(Default)]
struct Args {
    game: Option<PathBuf>,
    patch: Option<PathBuf>,
    yes: bool,
    no_backup: bool,
    out: Option<PathBuf>,
    verify: bool,
    revert: bool,
    info: bool,
}

fn parse() -> Result<Option<Args>, String> {
    let mut a = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy();
        let mut value = |name: &str| it.next().map(PathBuf::from).ok_or(format!("{name} needs a value"));
        match s.as_ref() {
            "-h" | "--help" => return Ok(None),
            "-y" | "--yes" => a.yes = true,
            "--no-backup" => a.no_backup = true,
            "--verify" => a.verify = true,
            "--revert" => a.revert = true,
            "--info" => a.info = true,
            "--patch" => a.patch = Some(value("--patch")?),
            "--out" => a.out = Some(value("--out")?),
            _ if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ if a.game.is_none() => a.game = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {s}")),
        }
    }
    Ok(Some(a))
}

fn main() -> ExitCode {
    // Double-clicked (no arguments): keep the console window open at the end.
    let interactive = std::env::args_os().len() == 1;
    let code = match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nError: {e}");
            ExitCode::FAILURE
        }
    };
    if interactive {
        ui::prompt("\nPress Enter to exit...");
    }
    code
}

fn run() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(args) = parse()? else {
        let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        print!("{}", USAGE.replace("{exe}", &name));
        return Ok(());
    };
    let progress = Console::default();

    if args.revert {
        let game = args.game.clone().unwrap_or_else(|| {
            [exe_dir.clone(), std::env::current_dir().unwrap_or_default()]
                .into_iter()
                .find(|d| d.join(patcher_core::apply::BACKUP_DIR).is_dir())
                .unwrap_or_else(|| exe_dir.clone())
        });
        let (name, n) = patcher_core::revert(&game, &progress).map_err(|e| e.to_string())?;
        println!("Reverted {name} ({n} files restored) in {}", game.display());
        return Ok(());
    }

    let patch = load_patch(&exe, &exe_dir, args.patch.as_deref())?;
    println!("{}", ui::banner(&patch));
    if args.info {
        for w in &patch.index.writes {
            println!("  write   {} ({})", w.path, human_bytes(w.check.size));
        }
        for &d in &patch.index.deletes {
            println!("  delete  {}", patch.index.base_files[d as usize].path);
        }
        return Ok(());
    }

    let game = match args.game {
        Some(g) => g,
        None => {
            let cands = [exe_dir.clone(), std::env::current_dir().unwrap_or_default()];
            match ui::find_game_dir(&patch.index, &cands) {
                Some(d) => d,
                None => PathBuf::from(
                    ui::prompt("Game folder (drag it here and press Enter): ").filter(|s| !s.is_empty()).ok_or("no game folder given")?,
                ),
            }
        }
    };
    println!("Game folder: {}", game.display());

    if args.verify {
        return match patcher_core::inspect(&patch, &game, &progress).map_err(|e| e.to_string())? {
            GameState::Unpatched => {
                println!("The game matches; the patch can be applied.");
                Ok(())
            }
            GameState::Patched => {
                println!("The patch is already applied.");
                Ok(())
            }
            GameState::Mismatch(list) => Err(Error::BaseMismatch(list).to_string()),
        };
    }

    if !args.yes && !ui::confirm("Apply the patch?") {
        return Err("cancelled".into());
    }
    let report = match &args.out {
        Some(out) => patcher_core::apply_to_copy(&patch, &game, out, &progress),
        None => patcher_core::apply(&patch, &game, &ApplyOptions { backup: !args.no_backup, progress: &progress }),
    }
    .map_err(|e| e.to_string())?;
    println!(
        "Done: {} files written ({}), {} deleted.{}",
        report.written,
        human_bytes(report.written_bytes),
        report.deleted,
        if report.backed_up { " Originals backed up; run with --revert to undo." } else { "" }
    );
    Ok(())
}

fn load_patch(exe: &Path, exe_dir: &Path, explicit: Option<&Path>) -> Result<Patch, String> {
    if let Some(p) = explicit {
        return Patch::open(p).map_err(|e| format!("{}: {e}", p.display()));
    }
    if let Some(p) = Patch::open_embedded(exe).map_err(|e| e.to_string())? {
        return Ok(p);
    }
    let found: Vec<PathBuf> = std::fs::read_dir(exe_dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("patch")))
        .collect();
    match found.as_slice() {
        [one] => Patch::open(one).map_err(|e| format!("{}: {e}", one.display())),
        [] => Err("no patch found: this patcher has none bundled and there is no .patch file next to it".into()),
        _ => Err("several .patch files next to the patcher; choose one with --patch FILE".into()),
    }
}
