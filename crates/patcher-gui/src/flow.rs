//! The installer's screen flow as a plain state machine.
//!
//! [`Flow`] owns a [`State`] that mirrors the `Patcher` global. Every user action and every
//! result from background work goes through a method here, which updates the state and
//! returns the [`Effect`]s the runtime must carry out (start a check, apply, open a
//! dialog...). Nothing here touches Slint, threads or the file system, so the whole flow
//! is tested without a window.

use std::collections::HashMap;
use std::path::PathBuf;

use patcher_core::progress::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Welcome,
    License,
    Folder,
    Options,
    Progress,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseStatus {
    Unknown,
    Checking,
    Ok,
    AlreadyApplied,
    Mismatch,
    NotFound,
}

/// Mirrors the `Patcher` global (see `ui/api.slint`).
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub screen: Screen,
    pub mod_name: String,
    pub mod_version: String,
    pub mod_description: String,
    pub mod_url: String,
    pub byline: String,
    pub options_merged: bool,
    pub backup: bool,
    pub can_revert: bool,
    pub reverting: bool,
    pub license_text: String,
    pub require_accept: bool,
    pub license_accepted: bool,
    pub game_path: String,
    pub base_status: BaseStatus,
    pub base_detail: String,
    pub can_browse: bool,
    pub folder_error: String,
    pub can_cancel: bool,
    pub finishing: bool,
    pub result: String,
    pub can_launch: bool,
    pub error_text: String,
    pub error_details: String,
    pub can_back: bool,
    pub can_next: bool,
    pub next_label: String,
    pub can_close: bool,
}

/// What the theme and the patch say about the run.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub welcome: bool,
    pub options: bool,
    /// License text, when the theme has a license page.
    pub license: Option<String>,
    pub require_accept: bool,
    /// A launch target is configured (checked again against the folder at the end).
    pub launch: bool,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub url: String,
    /// Every string, already filled with the name, version and author.
    pub strings: HashMap<String, String>,
}

/// How the run starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The normal wizard, with the folder that was detected (if any).
    Wizard(Option<PathBuf>),
    /// Relaunched as administrator: go straight to work.
    Elevated { game: PathBuf, job: Job },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Apply { backup: bool },
    Revert,
}

/// Result of checking a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inspection {
    Unpatched { writable: bool },
    Patched { writable: bool, can_revert: bool },
    Mismatch(Vec<String>),
    NotFound,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    AlreadyApplied,
    Reverted,
    Cancelled,
    Failed { message: String, details: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Check `path`; answer with [`Flow::inspected`] carrying the same `generation`.
    Inspect { path: PathBuf, generation: u64 },
    Run { game: PathBuf, job: Job },
    Elevate { game: PathBuf, job: Job },
    Cancel,
    Browse,
    Launch(PathBuf),
    OpenFolder(PathBuf),
    OpenUrl(String),
    Copy(String),
    Minimize,
    DragWindow,
    Close,
}

pub struct Flow {
    pub state: State,
    config: Config,
    screens: Vec<Screen>,
    generation: u64,
    writable: bool,
    close_after_cancel: bool,
    /// Where cancel returns to.
    last_wizard: Screen,
}

impl Flow {
    pub fn new(config: Config, start: Start) -> (Flow, Vec<Effect>) {
        let options_merged = config.options && config.welcome && config.license.is_none();
        let mut screens = Vec::new();
        if config.welcome {
            screens.push(Screen::Welcome);
        }
        if config.license.is_some() {
            screens.push(Screen::License);
        }
        screens.push(Screen::Folder);
        if config.options && !options_merged {
            screens.push(Screen::Options);
        }
        let byline = [
            (!config.version.is_empty()).then(|| text(&config, "version_label")),
            (!config.author.is_empty()).then(|| text(&config, "by_author")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("  ·  ");
        let state = State {
            screen: screens[0],
            mod_name: config.name.clone(),
            mod_version: config.version.clone(),
            mod_description: config.description.clone(),
            mod_url: config.url.clone(),
            byline,
            options_merged,
            backup: true,
            can_revert: false,
            reverting: false,
            license_text: config.license.clone().unwrap_or_default(),
            require_accept: config.require_accept,
            license_accepted: false,
            game_path: String::new(),
            base_status: BaseStatus::Unknown,
            base_detail: String::new(),
            can_browse: true,
            folder_error: String::new(),
            can_cancel: false,
            finishing: false,
            result: String::new(),
            can_launch: false,
            error_text: String::new(),
            error_details: String::new(),
            can_back: false,
            can_next: false,
            next_label: String::new(),
            can_close: true,
        };
        let mut f = Flow {
            last_wizard: state.screen,
            state,
            config,
            screens,
            generation: 0,
            writable: true,
            close_after_cancel: false,
        };
        let effects = match start {
            Start::Wizard(path) => match path {
                Some(p) => f.set_game_path(p.to_string_lossy().into_owned()),
                None => {
                    f.refresh();
                    vec![]
                }
            },
            Start::Elevated { game, job } => {
                f.state.game_path = game.to_string_lossy().into_owned();
                f.last_wizard = Screen::Folder;
                f.begin(job)
            }
        };
        (f, effects)
    }

    fn s(&self, key: &str) -> String {
        text(&self.config, key)
    }

    fn game(&self) -> PathBuf {
        PathBuf::from(self.state.game_path.trim())
    }

    fn position(&self) -> usize {
        self.screens.iter().position(|s| *s == self.state.screen).unwrap_or(0)
    }

    fn is_last_wizard_screen(&self) -> bool {
        self.position() + 1 == self.screens.len()
    }

    /// Recomputes the navigation fields from the rest of the state.
    fn refresh(&mut self) {
        let st = &mut self.state;
        let wizard = !matches!(st.screen, Screen::Progress | Screen::Done | Screen::Error);
        st.can_back = wizard && self.screens.iter().position(|s| *s == st.screen).is_some_and(|i| i > 0);
        let folder_seen = self.screens.iter().position(|s| *s == Screen::Folder) <= self.screens.iter().position(|s| *s == st.screen);
        let last = self.screens.last() == Some(&st.screen);
        let ready = st.base_status == BaseStatus::Ok;
        let revert_ready = st.base_status == BaseStatus::AlreadyApplied && st.can_revert;
        st.can_next = match st.screen {
            Screen::License => !st.require_accept || st.license_accepted,
            Screen::Folder => ready || revert_ready,
            Screen::Options => ready,
            _ => wizard,
        };
        st.next_label = if st.screen == Screen::Folder && revert_ready {
            text(&self.config, "revert")
        } else if last && folder_seen {
            text(&self.config, if self.writable { "patch" } else { "patch_admin" })
        } else {
            text(&self.config, "next")
        };
        if !matches!(st.screen, Screen::Progress) {
            st.can_close = true;
        }
    }

    // Named after the `Patcher.next` callback it handles.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Vec<Effect> {
        if !self.state.can_next {
            return vec![];
        }
        let st = &self.state;
        if st.screen == Screen::Folder && st.base_status == BaseStatus::AlreadyApplied {
            return self.revert();
        }
        if self.is_last_wizard_screen() {
            return self.start();
        }
        self.state.screen = self.screens[self.position() + 1];
        self.state.folder_error.clear();
        self.refresh();
        vec![]
    }

    pub fn back(&mut self) -> Vec<Effect> {
        if self.state.can_back {
            self.state.screen = self.screens[self.position() - 1];
            self.refresh();
        }
        vec![]
    }

    pub fn set_game_path(&mut self, path: String) -> Vec<Effect> {
        self.state.game_path = path;
        self.state.folder_error.clear();
        self.generation += 1;
        let p = self.game();
        if p.as_os_str().is_empty() {
            self.state.base_status = BaseStatus::Unknown;
            self.state.base_detail.clear();
            self.refresh();
            return vec![];
        }
        self.state.base_status = BaseStatus::Checking;
        self.state.base_detail.clear();
        self.refresh();
        vec![Effect::Inspect { path: p, generation: self.generation }]
    }

    pub fn inspected(&mut self, generation: u64, result: Inspection) -> Vec<Effect> {
        if generation != self.generation {
            return vec![];
        }
        let st = &mut self.state;
        st.base_detail.clear();
        st.can_revert = false;
        match result {
            Inspection::Unpatched { writable } => {
                st.base_status = BaseStatus::Ok;
                self.writable = writable;
            }
            Inspection::Patched { writable, can_revert } => {
                st.base_status = BaseStatus::AlreadyApplied;
                st.can_revert = can_revert;
                self.writable = writable;
            }
            Inspection::Mismatch(list) => {
                st.base_status = BaseStatus::Mismatch;
                let mut shown: Vec<String> = list.iter().take(5).cloned().collect();
                if list.len() > 5 {
                    shown.push(format!("and {} more", list.len() - 5));
                }
                st.base_detail = shown.join("\n");
            }
            Inspection::NotFound => st.base_status = BaseStatus::NotFound,
            Inspection::Failed(e) => {
                st.base_status = BaseStatus::NotFound;
                st.base_detail = e;
            }
        }
        self.refresh();
        vec![]
    }

    pub fn browse(&mut self) -> Vec<Effect> {
        vec![Effect::Browse]
    }

    /// The folder picker's answer; `None` when it was dismissed, `Err` when there is no
    /// picker on this system.
    pub fn browsed(&mut self, picked: Result<Option<PathBuf>, ()>) -> Vec<Effect> {
        match picked {
            Ok(Some(p)) => self.set_game_path(p.to_string_lossy().into_owned()),
            Ok(None) => vec![],
            Err(()) => {
                self.state.can_browse = false;
                vec![]
            }
        }
    }

    pub fn set_backup(&mut self, on: bool) -> Vec<Effect> {
        self.state.backup = on;
        vec![]
    }

    pub fn accept_license(&mut self, on: bool) -> Vec<Effect> {
        self.state.license_accepted = on;
        self.refresh();
        vec![]
    }

    fn start(&mut self) -> Vec<Effect> {
        let job = Job::Apply { backup: self.state.backup };
        if !self.writable {
            return vec![Effect::Elevate { game: self.game(), job }];
        }
        self.last_wizard = self.state.screen;
        self.begin(job)
    }

    pub fn revert(&mut self) -> Vec<Effect> {
        if !self.writable {
            return vec![Effect::Elevate { game: self.game(), job: Job::Revert }];
        }
        self.last_wizard = self.state.screen;
        self.begin(Job::Revert)
    }

    fn begin(&mut self, job: Job) -> Vec<Effect> {
        let st = &mut self.state;
        st.screen = Screen::Progress;
        st.reverting = job == Job::Revert;
        st.finishing = false;
        st.can_cancel = job != Job::Revert;
        st.can_close = st.can_cancel;
        st.folder_error.clear();
        self.refresh();
        vec![Effect::Run { game: self.game(), job }]
    }

    /// A phase started in the running job.
    pub fn phase(&mut self, phase: Phase) -> Vec<Effect> {
        if matches!(phase, Phase::Commit | Phase::Revert) {
            self.state.finishing = phase == Phase::Commit;
            self.state.can_cancel = false;
            self.state.can_close = false;
        }
        vec![]
    }

    pub fn cancel(&mut self) -> Vec<Effect> {
        if self.state.screen == Screen::Progress && self.state.can_cancel {
            self.state.can_cancel = false;
            return vec![Effect::Cancel];
        }
        vec![]
    }

    pub fn finished(&mut self, outcome: Outcome, launch_target: Option<PathBuf>) -> Vec<Effect> {
        let st = &mut self.state;
        st.can_cancel = false;
        st.finishing = false;
        st.can_close = true;
        let close = std::mem::take(&mut self.close_after_cancel);
        match outcome {
            Outcome::Applied | Outcome::AlreadyApplied | Outcome::Reverted => {
                let key = match outcome {
                    Outcome::Applied => "done_installed",
                    Outcome::AlreadyApplied => "done_already",
                    _ => "done_reverted",
                };
                self.state.result = text(&self.config, key);
                self.state.can_launch = self.config.launch && launch_target.is_some() && outcome != Outcome::Reverted;
                self.state.screen = Screen::Done;
            }
            Outcome::Cancelled => {
                self.state.screen = self.last_wizard;
                self.state.folder_error = text(&self.config, "cancelled");
                if close {
                    return vec![Effect::Close];
                }
            }
            Outcome::Failed { message, details } => {
                self.state.error_text = message;
                self.state.error_details = details;
                self.state.screen = Screen::Error;
            }
        }
        self.refresh();
        vec![]
    }

    pub fn elevation_declined(&mut self) -> Vec<Effect> {
        self.state.folder_error = self.s("elevation_declined");
        vec![]
    }

    pub fn close(&mut self) -> Vec<Effect> {
        if !self.state.can_close {
            return vec![];
        }
        if self.state.screen == Screen::Progress {
            self.close_after_cancel = true;
            return self.cancel();
        }
        vec![Effect::Close]
    }

    pub fn launch(&mut self, target: PathBuf) -> Vec<Effect> {
        vec![Effect::Launch(target)]
    }

    pub fn open_folder(&mut self) -> Vec<Effect> {
        vec![Effect::OpenFolder(self.game())]
    }

    pub fn open_url(&mut self, url: String) -> Vec<Effect> {
        vec![Effect::OpenUrl(url)]
    }

    pub fn copy_error(&mut self) -> Vec<Effect> {
        let st = &self.state;
        vec![Effect::Copy(format!("{}\n\n{}", st.error_text, st.error_details).trim().to_string())]
    }
}

fn text(config: &Config, key: &str) -> String {
    config.strings.get(key).cloned().unwrap_or_else(|| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::{fill, STRINGS};

    fn config(license: bool, accept: bool) -> Config {
        Config {
            welcome: true,
            options: true,
            license: license.then(|| "terms".to_string()),
            require_accept: accept,
            launch: true,
            name: "Mod".into(),
            version: "1.0".into(),
            author: "me".into(),
            strings: STRINGS.iter().map(|(k, v)| (k.to_string(), fill(v, "Mod", "1.0", "me"))).collect(),
            ..Default::default()
        }
    }

    fn game() -> PathBuf {
        PathBuf::from("/games/x")
    }

    /// Starts with a detected folder and answers its inspection with `answer`.
    fn started(c: Config, answer: Inspection) -> Flow {
        let (mut f, fx) = Flow::new(c, Start::Wizard(Some(game())));
        assert_eq!(fx, vec![Effect::Inspect { path: game(), generation: 1 }]);
        f.inspected(1, answer);
        f
    }

    #[test]
    fn normal_run_merges_options_into_welcome() {
        let mut f = started(config(false, false), Inspection::Unpatched { writable: true });
        assert!(f.state.options_merged);
        assert_eq!(f.state.byline, "Version 1.0  ·  by me");
        assert_eq!((f.state.screen, f.state.can_back), (Screen::Welcome, false));
        f.next();
        assert_eq!(f.state.screen, Screen::Folder);
        assert_eq!((f.state.next_label.as_str(), f.state.can_next), ("Patch", true));
        f.set_backup(false);
        assert_eq!(f.next(), vec![Effect::Run { game: game(), job: Job::Apply { backup: false } }]);
        assert_eq!((f.state.screen, f.state.can_cancel, f.state.can_close), (Screen::Progress, true, true));
        f.phase(Phase::Commit);
        assert_eq!((f.state.finishing, f.state.can_cancel, f.state.can_close), (true, false, false));
        assert_eq!(f.close(), vec![], "close refused during commit");
        f.finished(Outcome::Applied, Some(game().join("game.exe")));
        assert_eq!(f.state.screen, Screen::Done);
        assert_eq!(f.state.result, "Mod is installed.");
        assert!(f.state.can_launch && f.state.can_close);
        assert_eq!(f.close(), vec![Effect::Close]);
    }

    #[test]
    fn license_gate_and_separate_options() {
        let mut f = started(config(true, true), Inspection::Unpatched { writable: true });
        assert!(!f.state.options_merged);
        f.next();
        assert_eq!((f.state.screen, f.state.can_next), (Screen::License, false));
        assert_eq!(f.next(), vec![]);
        f.accept_license(true);
        f.next();
        assert_eq!((f.state.screen, f.state.next_label.as_str()), (Screen::Folder, "Next"));
        f.next();
        assert_eq!((f.state.screen, f.state.next_label.as_str()), (Screen::Options, "Patch"));
        f.back();
        assert_eq!(f.state.screen, Screen::Folder);
    }

    #[test]
    fn mismatch_blocks_and_lists_files() {
        let list: Vec<String> = (0..8).map(|i| format!("changed: f{i}")).collect();
        let mut f = started(config(false, false), Inspection::Mismatch(list));
        f.next();
        assert_eq!((f.state.base_status, f.state.can_next), (BaseStatus::Mismatch, false));
        assert_eq!(f.state.base_detail.lines().count(), 6);
        assert!(f.state.base_detail.ends_with("and 3 more"));
        assert_eq!(f.next(), vec![]);
    }

    #[test]
    fn stale_inspections_are_ignored() {
        let (mut f, _) = Flow::new(config(false, false), Start::Wizard(None));
        f.set_game_path("/a".into());
        f.set_game_path("/b".into());
        f.inspected(1, Inspection::Unpatched { writable: true });
        assert_eq!(f.state.base_status, BaseStatus::Checking);
        f.inspected(2, Inspection::NotFound);
        assert_eq!(f.state.base_status, BaseStatus::NotFound);
    }

    #[test]
    fn already_applied_offers_revert() {
        let mut f = started(config(false, false), Inspection::Patched { writable: true, can_revert: true });
        assert!(f.state.can_revert);
        f.next();
        assert_eq!((f.state.next_label.as_str(), f.state.can_next), ("Remove mod", true));
        assert_eq!(f.next(), vec![Effect::Run { game: game(), job: Job::Revert }]);
        assert!(f.state.reverting && !f.state.can_cancel);
        f.finished(Outcome::Reverted, None);
        assert_eq!((f.state.screen, f.state.can_launch), (Screen::Done, false));
        assert!(f.state.result.contains("removed"));

        let mut g = started(config(false, false), Inspection::Patched { writable: true, can_revert: false });
        g.next();
        assert!(!g.state.can_next, "no backup: nothing to do");
    }

    #[test]
    fn cancel_returns_to_the_wizard() {
        let mut f = started(config(false, false), Inspection::Unpatched { writable: true });
        f.next();
        f.next();
        assert_eq!(f.cancel(), vec![Effect::Cancel]);
        assert_eq!(f.cancel(), vec![], "only once");
        f.finished(Outcome::Cancelled, None);
        assert_eq!(f.state.screen, Screen::Folder);
        assert_eq!(f.state.folder_error, "Cancelled; nothing was changed.");

        // Closing during the run cancels first, then closes.
        f.next();
        assert_eq!(f.close(), vec![Effect::Cancel]);
        assert_eq!(f.finished(Outcome::Cancelled, None), vec![Effect::Close]);
    }

    #[test]
    fn failures_show_the_error_screen() {
        let mut f = started(config(false, false), Inspection::Unpatched { writable: true });
        f.next();
        f.next();
        f.finished(Outcome::Failed { message: "disk full".into(), details: "os error 112".into() }, None);
        assert_eq!((f.state.screen, f.state.error_text.as_str()), (Screen::Error, "disk full"));
        assert_eq!(f.copy_error(), vec![Effect::Copy("disk full\n\nos error 112".into())]);
    }

    #[test]
    fn unwritable_folders_elevate() {
        let mut f = started(config(false, false), Inspection::Unpatched { writable: false });
        f.next();
        assert_eq!(f.state.next_label, "Patch as administrator");
        assert_eq!(f.next(), vec![Effect::Elevate { game: game(), job: Job::Apply { backup: true } }]);
        assert_eq!(f.state.screen, Screen::Folder, "stays until the elevated copy runs");
        f.elevation_declined();
        assert_eq!(f.state.folder_error, "Administrator access was declined.");

        let (g, fx) = Flow::new(config(false, false), Start::Elevated { game: game(), job: Job::Apply { backup: false } });
        assert_eq!(g.state.screen, Screen::Progress);
        assert_eq!(fx, vec![Effect::Run { game: game(), job: Job::Apply { backup: false } }]);
    }

    #[test]
    fn no_picker_hides_browse() {
        let (mut f, _) = Flow::new(config(false, false), Start::Wizard(None));
        f.browsed(Err(()));
        assert!(!f.state.can_browse);
        assert_eq!(f.browsed(Ok(Some(game()))), vec![Effect::Inspect { path: game(), generation: 1 }]);
    }

    #[test]
    fn without_welcome_the_folder_comes_first() {
        let mut c = config(false, false);
        c.welcome = false;
        let (f, _) = Flow::new(c, Start::Wizard(None));
        assert_eq!(f.state.screen, Screen::Folder);
        assert!(!f.state.options_merged);
        assert_eq!(f.state.next_label, "Next", "options come after");
    }
}
