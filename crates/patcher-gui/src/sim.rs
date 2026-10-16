//! Backends for `gui-preview`: a scripted run with no patch, and a verify-only run of a
//! real patch. Neither writes anything.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use patcher_core::format::Meta;
use patcher_core::progress::{Phase, Progress};
use patcher_core::Patch;

use crate::backend::{Backend, PatchBackend};
use crate::flow::{Inspection, Job, Outcome};

const FILES: &[&str] = &[
    "data/levels/world_01.pak",
    "data/levels/world_02.pak",
    "data/textures/ui.atlas",
    "data/sound/music.bank",
    "bin/game.dll",
    "data/scripts/main.lua",
];

/// An 8-second pretend install of [`crate::snapshot::demo_meta`]. With `fail`, the run
/// ends on the error screen.
pub struct SimBackend {
    pub meta: Meta,
    pub fail: bool,
}

impl SimBackend {
    fn phase(&self, p: &dyn Progress, cancel: &AtomicBool, phase: Phase, total: u64, steps: u64, step: Duration) -> bool {
        p.begin(phase, total);
        for i in 0..steps {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            if phase.is_bytes() {
                p.file(FILES[i as usize % FILES.len()]);
            }
            std::thread::sleep(step);
            p.inc(total / steps);
        }
        p.end();
        true
    }
}

impl Backend for SimBackend {
    fn meta(&self) -> Meta {
        self.meta.clone()
    }

    fn looks_like_game(&self, _: &Path) -> bool {
        true
    }

    fn inspect(&self, _: &Path, _: &AtomicBool) -> Inspection {
        std::thread::sleep(Duration::from_millis(250));
        Inspection::Unpatched { writable: true }
    }

    fn run(&self, _: &Path, job: Job, p: &dyn Progress, cancel: &AtomicBool) -> Outcome {
        let ms = Duration::from_millis;
        if job == Job::Revert {
            self.phase(p, cancel, Phase::Revert, 40, 40, ms(40));
            return Outcome::Reverted;
        }
        p.note("Preview: nothing is written to disk");
        if !self.phase(p, cancel, Phase::Verify, 600 << 20, 40, ms(50)) {
            return Outcome::Cancelled;
        }
        if self.fail {
            self.phase(p, cancel, Phase::Decode, 400 << 20, 30, ms(80));
            return Outcome::Failed {
                message: "data/levels/world_02.pak: The process cannot access the file because it is being used by another process. (os error 32)".into(),
                details: "Simulated failure (gui-preview --fail).\n".into(),
            };
        }
        if !self.phase(p, cancel, Phase::Decode, 900 << 20, 60, ms(85)) {
            return Outcome::Cancelled;
        }
        self.phase(p, cancel, Phase::Commit, 120, 20, ms(40));
        Outcome::Applied
    }

    fn phase_totals(&self, job: Job) -> Vec<(Phase, u64)> {
        match job {
            Job::Revert => vec![(Phase::Revert, 40)],
            Job::Apply { .. } => vec![(Phase::Verify, 600 << 20), (Phase::Decode, 900 << 20), (Phase::Commit, 120)],
        }
    }
}

/// A real patch whose "apply" only verifies the game.
pub struct VerifyOnly(pub PatchBackend);

impl VerifyOnly {
    pub fn new(patch: Patch) -> VerifyOnly {
        VerifyOnly(PatchBackend(patch))
    }
}

impl Backend for VerifyOnly {
    fn meta(&self) -> Meta {
        self.0.meta()
    }

    fn looks_like_game(&self, dir: &Path) -> bool {
        self.0.looks_like_game(dir)
    }

    fn inspect(&self, dir: &Path, cancel: &AtomicBool) -> Inspection {
        // Never offer elevation in a preview.
        match self.0.inspect(dir, cancel) {
            Inspection::Unpatched { .. } => Inspection::Unpatched { writable: true },
            Inspection::Patched { can_revert, .. } => Inspection::Patched { writable: true, can_revert },
            other => other,
        }
    }

    fn run(&self, game: &Path, _: Job, p: &dyn Progress, cancel: &AtomicBool) -> Outcome {
        p.note("Preview: only verifying, nothing is written to disk");
        match patcher_core::inspect_with(&self.0 .0, game, p, Some(cancel)) {
            Ok(_) => Outcome::Applied,
            Err(patcher_core::Error::Cancelled) => Outcome::Cancelled,
            Err(e) => Outcome::Failed { message: e.to_string(), details: String::new() },
        }
    }

    fn phase_totals(&self, _: Job) -> Vec<(Phase, u64)> {
        patcher_core::phase_totals(&self.0 .0)[..1].to_vec()
    }
}
