//! `patcher create` with GUI patchers: themes are checked and bundled, the footer locates
//! bundle and patch, missing stubs fall back as documented.

use std::path::{Path, PathBuf};
use std::process::Command;

use patcher_core::format::Footer;

fn root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("patcher-cli-gui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn put(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// A tiny game, its mod, a stub folder with fake stubs (`names`), and a theme folder.
fn setup(tag: &str, names: &[&str], theme: &[(&str, &str)]) -> PathBuf {
    let r = root(tag);
    put(&r.join("base/game.dat"), &b"original game data ".repeat(200));
    put(&r.join("base/keep.txt"), b"same");
    put(&r.join("mod/game.dat"), &b"modded game data!! ".repeat(200));
    put(&r.join("mod/keep.txt"), b"same");
    put(&r.join("mod/new.txt"), b"added by the mod");
    for n in names {
        put(&r.join("stubs").join(n), format!("fake stub {n}").as_bytes());
    }
    for (p, text) in theme {
        put(&r.join("theme").join(p), text.as_bytes());
    }
    r
}

fn create(r: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_patcher"))
        .args(["create", "--name", "Test Mod", "--mod-version", "1.0", "--profile", "fast"])
        .arg("--base")
        .arg(r.join("base"))
        .arg("--modded")
        .arg(r.join("mod"))
        .arg("-o")
        .arg(r.join("out"))
        .arg("--stub-dir")
        .arg(r.join("stubs"))
        .args(extra)
        .output()
        .unwrap()
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const ALL_STUBS: [&str; 4] = ["patcher-stub-windows.exe", "patcher-stub-linux", "patcher-stub-gui-windows.exe", "patcher-stub-gui-linux"];

const PROGRESS_PART: &str = r#"import { Patcher, ProgressBar } from "@patcher";
export component ProgressScreen inherits Rectangle {
    ProgressBar { value: Patcher.progress; }
}
"#;

#[test]
fn themed_gui_patchers_carry_bundle_and_patch() {
    let r = setup("themed", &ALL_STUBS, &[("theme.toml", "[theme]\naccent = \"#ff3366\"\n[layout.parts]\nprogress = \"ui/progress.slint\"\n"), ("ui/progress.slint", PROGRESS_PART)]);
    let theme = r.join("theme/theme.toml");
    let o = create(&r, &["--gui", theme.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    for exe in ["TestMod-patcher.exe", "TestMod-patcher"] {
        let bytes = std::fs::read(r.join("out").join(exe)).unwrap();
        let f = Footer::find(&bytes).unwrap_or_else(|| panic!("{exe}: no footer"));
        assert!(f.ui.is_some() && f.patch.is_some(), "{exe}: {f:?}");
        let stub = &bytes[..f.ui.unwrap().offset as usize];
        let expect: &[u8] = if exe.ends_with(".exe") { b"fake stub patcher-stub-gui-windows.exe" } else { b"fake stub patcher-stub-gui-linux" };
        assert_eq!(stub, expect, "{exe}: wrong stub (fake stubs are not PE files, so no branding)");
        let path = patcher_gui::bundle::extract(Footer::ui_bytes(&bytes).unwrap(), &r.join("extract")).unwrap();
        let t = patcher_gui::Theme::load(&path).unwrap();
        assert_eq!(t.palette["accent"], patcher_gui::color::Color::rgb(0xff3366));
        patcher_gui::compile::compile(&t, &r.join("work")).unwrap();
    }
    assert!(stderr(&o).contains("could not set the Windows icon"), "fake stub cannot be branded: {}", stderr(&o));
}

#[test]
fn detached_patchers_have_no_patch_inside() {
    let r = setup("detached", &ALL_STUBS, &[]);
    let o = create(&r, &["--detached", "--target", "windows"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let bytes = std::fs::read(r.join("out/TestMod-patcher.exe")).unwrap();
    assert_eq!(Footer::find(&bytes), Some(Footer { ui: None, patch: None }), "default theme: no bundle, no patch");
    assert!(r.join("out/TestMod.patch").is_file());
}

#[test]
fn missing_linux_gui_stub_falls_back_to_console() {
    let r = setup("fallback", &["patcher-stub-windows.exe", "patcher-stub-linux", "patcher-stub-gui-windows.exe"], &[("theme.toml", "")]);
    let theme = r.join("theme/theme.toml");
    let o = create(&r, &["--gui", theme.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("no Linux GUI stub in this build"), "{}", stderr(&o));
    let bytes = std::fs::read(r.join("out/TestMod-patcher")).unwrap();
    let f = Footer::find(&bytes).unwrap();
    assert_eq!(f.ui, None, "console patchers carry no theme");
    assert_eq!(&bytes[..f.patch.unwrap().offset as usize], b"fake stub patcher-stub-linux");
}

#[test]
fn broken_themes_stop_create() {
    let r = setup("broken", &ALL_STUBS, &[("theme.toml", "[layout.parts]\ndone = \"done.slint\"\n"), ("done.slint", "export component DoneScreen inherits Rectangle {\n    nonsense: 3px;\n}\n")]);
    let theme = r.join("theme/theme.toml");
    let o = create(&r, &["--gui", theme.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("done.slint:2"), "{}", stderr(&o));
    assert!(!r.join("out").exists(), "nothing written");
}

#[test]
fn console_patchers_on_request() {
    let r = setup("cli", &ALL_STUBS, &[]);
    let o = create(&r, &["--ui", "cli", "--target", "linux"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let bytes = std::fs::read(r.join("out/TestMod-patcher")).unwrap();
    let f = Footer::find(&bytes).unwrap();
    assert_eq!(&bytes[..f.patch.unwrap().offset as usize], b"fake stub patcher-stub-linux");
}
