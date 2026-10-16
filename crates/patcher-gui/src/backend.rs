//! What the installer runs: a real patch, or a simulation for previews.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use patcher_core::format::Meta;
use patcher_core::progress::{Phase, Progress};
use patcher_core::{ApplyOptions, Error, GameState, Patch};

use crate::flow::{Inspection, Job, Outcome};

pub trait Backend: Send + Sync + 'static {
    fn meta(&self) -> Meta;
    /// Cheap check for game-folder detection (paths only, no hashing).
    fn looks_like_game(&self, dir: &Path) -> bool;
    fn inspect(&self, dir: &Path, cancel: &AtomicBool) -> Inspection;
    fn run(&self, game: &Path, job: Job, progress: &dyn Progress, cancel: &AtomicBool) -> Outcome;
    /// Totals each phase of `job` will report, for one weighted progress bar.
    fn phase_totals(&self, job: Job) -> Vec<(Phase, u64)>;
}

/// Applies a real patch.
pub struct PatchBackend(pub Patch);

impl Backend for PatchBackend {
    fn meta(&self) -> Meta {
        self.0.index.meta.clone()
    }

    /// The unpatched game, or one this patch already modified (some base files may be
    /// gone then, so its backup or its outputs count too).
    fn looks_like_game(&self, dir: &Path) -> bool {
        let idx = &self.0.index;
        let outputs_present = || {
            let probe: Vec<_> = idx.writes.iter().take(16).collect();
            !probe.is_empty() && probe.iter().all(|w| matches!(patcher_core::paths::resolve_existing(dir, &w.path), Ok(Some(_))))
        };
        patcher_core::ui::looks_like_game(idx, dir)
            || patcher_core::apply::backup_owner(dir).is_some_and(|(name, _)| name == idx.meta.name)
            || outputs_present()
    }

    fn inspect(&self, dir: &Path, cancel: &AtomicBool) -> Inspection {
        if !dir.is_dir() || !self.looks_like_game(dir) {
            return Inspection::NotFound;
        }
        let writable = crate::platform::writable(dir);
        match patcher_core::inspect_with(&self.0, dir, &patcher_core::progress::Silent, Some(cancel)) {
            Ok(GameState::Unpatched) => Inspection::Unpatched { writable },
            Ok(GameState::Patched) => {
                let owner = patcher_core::apply::backup_owner(dir);
                let can_revert = owner.is_some_and(|(name, _)| name == self.0.index.meta.name);
                Inspection::Patched { writable, can_revert }
            }
            Ok(GameState::Mismatch(list)) => Inspection::Mismatch(list),
            Err(e) => Inspection::Failed(e.to_string()),
        }
    }

    fn run(&self, game: &Path, job: Job, progress: &dyn Progress, cancel: &AtomicBool) -> Outcome {
        let r = match job {
            Job::Apply { backup } => {
                patcher_core::apply(&self.0, game, &ApplyOptions { backup, progress, cancel: Some(cancel) }).map(|_| Outcome::Applied)
            }
            Job::Revert => patcher_core::revert(game, progress).map(|_| Outcome::Reverted),
        };
        match r {
            Ok(o) => o,
            Err(Error::AlreadyApplied) => Outcome::AlreadyApplied,
            Err(Error::Cancelled) => Outcome::Cancelled,
            Err(e) => Outcome::Failed { message: e.to_string(), details: String::new() },
        }
    }

    fn phase_totals(&self, job: Job) -> Vec<(Phase, u64)> {
        match job {
            Job::Apply { .. } => patcher_core::phase_totals(&self.0).to_vec(),
            Job::Revert => vec![(Phase::Revert, 1)],
        }
    }
}
