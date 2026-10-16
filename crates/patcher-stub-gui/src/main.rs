//! End-user patcher with an installer window.
//!
//! Without arguments it opens the window. `--elevated --game DIR [--no-backup] [--revert]`
//! is the administrator relaunch and opens the window straight on the progress screen.
//! Any other arguments run the console patcher (the same code as the CLI stub), printing
//! to the terminal it was started from.

#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use patcher_gui::app::{self, RunOptions, StartMode};
use patcher_gui::backend::PatchBackend;
use patcher_gui::flow::Job;
use patcher_gui::{bundle, Theme};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args[0] == "--elevated" {
        match gui(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                patcher_gui::platform::message_box("Patcher", &e);
                ExitCode::FAILURE
            }
        }
    } else {
        attach_console();
        patcher_stub::run_cli(args, false)
    }
}

#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain Win32 call; failing (no parent console) just leaves output unseen.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    println!();
}

#[cfg(not(windows))]
fn attach_console() {}

/// `--elevated --game DIR [--no-backup] [--revert]`.
fn elevated(args: &[OsString]) -> Result<Option<(PathBuf, Job)>, String> {
    if args.is_empty() {
        return Ok(None);
    }
    let mut game = None;
    let mut job = Job::Apply { backup: true };
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--game") => game = it.next().map(PathBuf::from),
            Some("--no-backup") => job = Job::Apply { backup: false },
            Some("--revert") => job = Job::Revert,
            _ => return Err(format!("unexpected argument {}", a.to_string_lossy())),
        }
    }
    Ok(Some((game.ok_or("--elevated needs --game")?, job)))
}

fn gui(args: &[OsString]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let start = match elevated(args)? {
        Some((game, job)) => StartMode::Elevated { game, job },
        None => StartMode::Detect { exe_dir: exe_dir.clone() },
    };
    let patch = patcher_stub::load_patch(&exe, &exe_dir, None)?;

    let temp = std::env::temp_dir().join(format!("patcher-gui-{}", std::process::id()));
    let theme = {
        let file = std::fs::File::open(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        // SAFETY: read-only map of our own executable, which cannot change while it runs.
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
        match patcher_core::format::Footer::ui_bytes(&map) {
            Some(ui) => {
                let path = bundle::extract(ui, &temp).map_err(|e| e.to_string())?;
                Theme::load(&path).map_err(|e| e.to_string())?
            }
            None => Theme::builtin(),
        }
    };
    let r = app::run(RunOptions { theme, backend: Arc::new(PatchBackend(patch)), start, work: temp.join("ui"), watch: None });
    let _ = std::fs::remove_dir_all(&temp);
    r
}
