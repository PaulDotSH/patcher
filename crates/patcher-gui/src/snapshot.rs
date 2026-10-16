//! Rendering every screen to PNG without a window system, for `gui-preview --screenshot`,
//! documentation images and visual checks.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use patcher_core::format::Meta;
use patcher_core::progress::Phase;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint_interpreter::ComponentHandle;

use crate::flow::{Flow, Inspection, Outcome, Start, State};
use crate::view::{self, Live};
use crate::Theme;

/// The mod shown by previews and screenshots when there is no real patch.
pub fn demo_meta() -> Meta {
    Meta {
        name: "patcher".into(),
        version: "1.0".into(),
        author: "PaulDotSH".into(),
        description: "A preview of the installer. Nothing is written to disk.".into(),
        url: "https://github.com/PaulDotSH/patcher".into(),
        created_by: String::new(),
    }
}

/// One state per screen worth showing, built by driving a real [`Flow`].
pub fn sample_states(theme: &Theme, meta: &Meta) -> Vec<(&'static str, State, Live)> {
    let config = view::config(theme, meta);
    let game = PathBuf::from(if cfg!(windows) { r"C:\Games\Example" } else { "/home/you/Games/Example" });
    let (mut f, _) = Flow::new(config.clone(), Start::Wizard(Some(game.clone())));
    f.inspected(1, Inspection::Unpatched { writable: true });
    let none = Live::default();
    let mut out = vec![("welcome", f.state.clone(), none.clone())];
    while f.state.screen != crate::flow::Screen::Folder {
        f.accept_license(true);
        f.next();
        if f.state.screen == crate::flow::Screen::License {
            out.push(("license", f.state.clone(), none.clone()));
        }
    }
    out.push(("folder", f.state.clone(), none.clone()));

    let (mut m, _) = Flow::new(config.clone(), Start::Wizard(Some(game.clone())));
    m.inspected(1, Inspection::Mismatch(vec!["changed: data/main.pak".into(), "missing: bin/game.dll".into()]));
    while m.state.screen != crate::flow::Screen::Folder {
        m.accept_license(true);
        m.next();
    }
    out.push(("folder-mismatch", m.state.clone(), none.clone()));

    if config.options && !f.state.options_merged {
        f.next();
        out.push(("options", f.state.clone(), none.clone()));
    }
    f.next();
    let live = Live {
        progress: 0.62,
        phase: Phase::Decode.label().into(),
        current_file: "data/levels/world_03.pak".into(),
        speed: "84.2 MB/s".into(),
        eta: "0:07".into(),
        log: vec!["Verified 1 204 game files".into()],
    };
    out.push(("progress", f.state.clone(), live.clone()));
    let mut done = Flow::new(config.clone(), Start::Wizard(None)).0;
    done.finished(Outcome::Applied, Some(game.join("game.exe")));
    out.push(("done", done.state.clone(), live));
    let mut err = Flow::new(config, Start::Wizard(None)).0;
    err.finished(
        Outcome::Failed {
            message: "C:\\Games\\Example\\data\\main.pak: The process cannot access the file because it is being used by another process. (os error 32)".into(),
            details: "Phase: Installing\nThe game may still be running. Close it and try again.".into(),
        },
        None,
    );
    out.push(("error", err.state.clone(), none));
    out
}

/// Renders every sample state of `theme` into `out/<screen>.png`. Must run before any other
/// window is created in this process: it installs a headless platform.
pub fn render_all(theme: &Theme, meta: &Meta, work: &Path, out: &Path) -> Result<Vec<PathBuf>, String> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    struct Headless(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
        fn duration_since_start(&self) -> Duration {
            Duration::ZERO
        }
    }
    slint::platform::set_platform(Box::new(Headless(window.clone()))).map_err(|e| e.to_string())?;
    let def = crate::compile::compile(theme, work).map_err(|e| e.to_string())?;
    let inst = def.create().map_err(|e| e.to_string())?;
    window.set_size(slint::PhysicalSize::new(theme.file.window.width, theme.file.window.height));
    let strings = view::strings(theme, &meta.name, &meta.version, &meta.author);
    view::apply_theme(&inst, theme, &strings, &meta.name);
    let _ = inst.set_global_property("Patcher", "tick", slint_interpreter::Value::Number(2600.0));
    inst.show().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    for (name, state, live) in sample_states(theme, meta) {
        view::apply_state(&inst, &state);
        view::apply_live(&inst, &live);
        let shot = inst.window().take_snapshot().map_err(|e| e.to_string())?;
        let path = out.join(format!("{name}.png"));
        write_png(&path, shot.width(), shot.height(), shot.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut e = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()?.write_image_data(rgba)?;
    Ok(())
}
