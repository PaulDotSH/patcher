//! The installer window: wires the compiled theme to the flow, the backend and the OS.
//!
//! Everything runs on the UI thread except checks, jobs and the folder picker, which run
//! on worker threads and post their results back with `invoke_from_event_loop`.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use patcher_core::progress::Phase;
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint_interpreter::{ComponentHandle, ComponentInstance, Value};

use crate::backend::Backend;
use crate::flow::{Effect, Flow, Job, Outcome, Start};
use crate::live::LiveProgress;
use crate::platform::{self, ElevateError};
use crate::view;
use crate::Theme;

pub enum StartMode {
    /// Look for the game in the theme's default paths, then next to the patcher.
    Detect { exe_dir: PathBuf },
    /// Relaunched as administrator for `job` on `game`.
    Elevated { game: PathBuf, job: Job },
}

pub struct RunOptions {
    pub theme: Theme,
    pub backend: Arc<dyn Backend>,
    pub start: StartMode,
    /// Where the Slint sources are written and compiled.
    pub work: PathBuf,
}

/// Arguments of the elevated relaunch for `job`, read back by the GUI stub.
pub fn elevated_args(game: &Path, job: Job) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--elevated".into(), "--game".into(), game.into()];
    match job {
        Job::Apply { backup: false } => args.push("--no-backup".into()),
        Job::Apply { backup: true } => {}
        Job::Revert => args.push("--revert".into()),
    }
    args
}

struct App {
    inst: ComponentInstance,
    flow: RefCell<Flow>,
    theme: Theme,
    backend: Arc<dyn Backend>,
    live: Arc<LiveProgress>,
    cancel: Arc<AtomicBool>,
    inspect_cancel: RefCell<Arc<AtomicBool>>,
    last_phase: Cell<Option<Phase>>,
    focused: Cell<bool>,
    minimized: Cell<bool>,
    started: Instant,
    debounce: slint::Timer,
    live_timer: slint::Timer,
    tick_timer: slint::Timer,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

/// Runs `f` on the UI thread with the app.
fn post(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(f));
}

/// First default path (expanded) that looks like the game, else the patcher's folder or
/// the current one if they do, else the first default path as a starting point.
pub fn detect_game(theme: &Theme, backend: &dyn Backend, exe_dir: &Path) -> Option<PathBuf> {
    let configured: Vec<PathBuf> = theme.default_paths().iter().filter_map(|s| crate::expand::expand(s)).collect();
    let cwd = std::env::current_dir().ok();
    configured
        .iter()
        .cloned()
        .chain([exe_dir.to_path_buf()])
        .chain(cwd)
        .find(|d| backend.looks_like_game(d))
        .or_else(|| configured.first().cloned())
}

pub fn run(opts: RunOptions) -> Result<(), String> {
    let def = crate::compile::compile(&opts.theme, &opts.work).map_err(|e| e.to_string())?;
    let inst = def.create().map_err(|e| e.to_string())?;
    let meta = opts.backend.meta();
    let config = view::config(&opts.theme, &meta);
    view::apply_theme(&inst, &opts.theme, &config.strings, &meta.name);
    let start = match opts.start {
        StartMode::Detect { exe_dir } => Start::Wizard(detect_game(&opts.theme, &*opts.backend, &exe_dir)),
        StartMode::Elevated { game, job } => Start::Elevated { game, job },
    };
    let (flow, effects) = Flow::new(config, start);
    let app = Rc::new(App {
        inst: inst.clone_strong(),
        flow: RefCell::new(flow),
        theme: opts.theme,
        backend: opts.backend,
        live: Arc::new(LiveProgress::default()),
        cancel: Arc::new(AtomicBool::new(false)),
        inspect_cancel: RefCell::new(Arc::new(AtomicBool::new(false))),
        last_phase: Cell::new(None),
        focused: Cell::new(true),
        minimized: Cell::new(false),
        started: Instant::now(),
        debounce: slint::Timer::default(),
        live_timer: slint::Timer::default(),
        tick_timer: slint::Timer::default(),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    register_callbacks(&inst);
    inst.window().on_winit_window_event(|w, ev| {
        match ev {
            winit::event::WindowEvent::Focused(f) => with_app(|a| {
                a.focused.set(*f);
                a.update_tick();
            }),
            winit::event::WindowEvent::Resized(_) => {
                let minimized = w.with_winit_window(|w| w.is_minimized()).flatten().unwrap_or(false);
                with_app(|a| {
                    a.minimized.set(minimized);
                    a.update_tick();
                });
            }
            _ => {}
        }
        EventResult::Propagate
    });
    inst.window().on_close_requested(|| {
        with_app(|a| a.dispatch(|f| f.close()));
        slint::CloseRequestResponse::KeepWindowShown
    });
    app.sync();
    for e in effects {
        app.execute(e);
    }
    drop(app);
    let r = inst.run().map_err(|e| e.to_string());
    APP.with(|a| a.borrow_mut().take());
    r
}

fn register_callbacks(inst: &ComponentInstance) {
    let on = |name: &str, f: fn(&Rc<App>, &[Value])| {
        let _ = inst.set_global_callback("Patcher", name, move |args| {
            with_app(|a| f(a, args));
            Value::Void
        });
    };
    on("next", |a, _| a.dispatch(|f| f.next()));
    on("start", |a, _| a.dispatch(|f| f.next()));
    on("back", |a, _| a.dispatch(|f| f.back()));
    on("set-game-path", |a, args| {
        let t = text(args);
        a.dispatch(|f| f.set_game_path(t))
    });
    on("browse", |a, _| a.dispatch(|f| f.browse()));
    on("set-backup", |a, args| {
        let v = flag(args);
        a.dispatch(|f| f.set_backup(v))
    });
    on("accept-license", |a, args| {
        let v = flag(args);
        a.dispatch(|f| f.accept_license(v))
    });
    on("cancel", |a, _| a.dispatch(|f| f.cancel()));
    on("revert", |a, _| a.dispatch(|f| f.revert()));
    on("launch", |a, _| a.launch());
    on("open-folder", |a, _| a.dispatch(|f| f.open_folder()));
    on("open-url", |a, args| {
        let t = text(args);
        a.dispatch(|f| f.open_url(t))
    });
    on("copy-error", |a, _| a.dispatch(|f| f.copy_error()));
    on("minimize", |a, _| a.execute(Effect::Minimize));
    on("close", |a, _| a.dispatch(|f| f.close()));
    on("drag-window", |a, _| a.execute(Effect::DragWindow));
}

fn text(args: &[Value]) -> String {
    match args.first() {
        Some(Value::String(s)) => s.to_string(),
        _ => String::new(),
    }
}

fn flag(args: &[Value]) -> bool {
    matches!(args.first(), Some(Value::Bool(true)))
}

impl App {
    fn dispatch(self: &Rc<Self>, f: impl FnOnce(&mut Flow) -> Vec<Effect>) {
        let effects = f(&mut self.flow.borrow_mut());
        self.sync();
        for e in effects {
            self.execute(e);
        }
    }

    fn sync(&self) {
        view::apply_state(&self.inst, &self.flow.borrow().state);
    }

    fn game(&self) -> PathBuf {
        PathBuf::from(self.flow.borrow().state.game_path.trim())
    }

    /// Animation runs at up to 30 fps while the window is visible and focused.
    fn update_tick(&self) {
        let want = self.focused.get() && !self.minimized.get();
        if want && !self.tick_timer.running() {
            let started = self.started;
            self.tick_timer.start(slint::TimerMode::Repeated, Duration::from_millis(33), move || {
                with_app(|a| {
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    let _ = a.inst.set_global_property("Patcher", "tick", Value::Number(ms));
                });
            });
        } else if !want {
            self.tick_timer.stop();
        }
    }

    fn execute(self: &Rc<Self>, effect: Effect) {
        match effect {
            Effect::Inspect { path, generation } => {
                let stop = Arc::new(AtomicBool::new(false));
                self.inspect_cancel.replace(stop.clone()).store(true, Ordering::Relaxed);
                let backend = self.backend.clone();
                // Debounced: typing a path only checks once the user pauses.
                self.debounce.start(slint::TimerMode::SingleShot, Duration::from_millis(300), move || {
                    let (backend, stop, path) = (backend.clone(), stop.clone(), path.clone());
                    std::thread::spawn(move || {
                        let r = backend.inspect(&path, &stop);
                        if !stop.load(Ordering::Relaxed) {
                            post(move |a| a.dispatch(|f| f.inspected(generation, r)));
                        }
                    });
                });
            }
            Effect::Run { game, job } => {
                self.cancel.store(false, Ordering::Relaxed);
                self.live.reset(self.backend.phase_totals(job));
                self.last_phase.set(None);
                self.live_timer.start(slint::TimerMode::Repeated, Duration::from_millis(66), || with_app(|a| a.poll_live()));
                let (backend, live, cancel) = (self.backend.clone(), self.live.clone(), self.cancel.clone());
                std::thread::spawn(move || {
                    let outcome = backend.run(&game, job, &*live, &cancel);
                    post(move |a| a.finished(outcome));
                });
            }
            Effect::Elevate { game, job } => match platform::relaunch_elevated(&elevated_args(&game, job)) {
                Ok(()) => self.execute(Effect::Close),
                Err(ElevateError::Declined) => self.dispatch(|f| f.elevation_declined()),
                Err(e) => {
                    let msg = match e {
                        ElevateError::Failed(m) => m,
                        _ => "this folder is not writable".into(),
                    };
                    self.flow.borrow_mut().state.folder_error = msg;
                    self.sync();
                }
            },
            Effect::Cancel => self.cancel.store(true, Ordering::Relaxed),
            Effect::Browse => {
                let start = self.game();
                std::thread::spawn(move || {
                    let mut dialog = rfd::FileDialog::new();
                    if start.is_dir() {
                        dialog = dialog.set_directory(&start);
                    }
                    let picked = dialog.pick_folder();
                    post(move |a| a.dispatch(|f| f.browsed(Ok(picked))));
                });
            }
            Effect::Launch(_) => self.launch(),
            Effect::OpenFolder(p) => platform::open(&p.to_string_lossy()),
            Effect::OpenUrl(u) => platform::open(&u),
            Effect::Copy(t) => platform::copy(&t),
            Effect::Minimize => self.inst.window().set_minimized(true),
            Effect::DragWindow => {
                self.inst.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
            Effect::Close => {
                let _ = self.inst.hide();
                let _ = slint::quit_event_loop();
            }
        }
    }

    fn poll_live(self: &Rc<Self>) {
        let snap = self.live.snapshot();
        view::apply_live(&self.inst, &snap);
        let phase = self.live.phase();
        if phase != self.last_phase.get() {
            self.last_phase.set(phase);
            if let Some(p) = phase {
                self.dispatch(|f| f.phase(p));
            }
        }
    }

    fn finished(self: &Rc<Self>, outcome: Outcome) {
        self.live_timer.stop();
        let mut snap = self.live.snapshot();
        if matches!(outcome, Outcome::Applied | Outcome::Reverted) {
            snap.progress = 1.0;
            snap.eta.clear();
        }
        view::apply_live(&self.inst, &snap);
        let outcome = match outcome {
            Outcome::Failed { message, details } => {
                let mut d = details;
                if !snap.phase.is_empty() {
                    d += &format!("Phase: {}\n", snap.phase);
                }
                if !snap.current_file.is_empty() {
                    d += &format!("File: {}\n", snap.current_file);
                }
                for l in self.live.log() {
                    d += &format!("{l}\n");
                }
                Outcome::Failed { message, details: d.trim_end().to_string() }
            }
            o => o,
        };
        let target = self.launch_target();
        self.dispatch(|f| f.finished(outcome, target));
    }

    fn launch_target(&self) -> Option<PathBuf> {
        let game = self.game();
        if self.theme.file.launch.steam_app_id.is_some() {
            return Some(game);
        }
        let p = patcher_core::paths::join(&game, &self.theme.launch_program()?.replace('\\', "/"));
        p.is_file().then_some(p)
    }

    fn launch(self: &Rc<Self>) {
        let game = self.game();
        if let Some(id) = self.theme.file.launch.steam_app_id {
            platform::open(&format!("steam://rungameid/{id}"));
        } else if let Some(program) = self.theme.launch_program()
            && let Err(e) = platform::launch(&game, program)
        {
            self.flow.borrow_mut().state.result += &format!("\n{program}: {e}");
            self.sync();
            return;
        }
        self.execute(Effect::Close);
    }
}
