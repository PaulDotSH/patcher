//! The documented themes load and compile, and the reference file lists every string.

use std::path::{Path, PathBuf};

use patcher_gui::compile::compile;
use patcher_gui::Theme;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn work(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("patcher-gui-examples-{tag}-{}", std::process::id()))
}

#[test]
fn example_themes_compile() {
    for (tag, file) in [
        ("reference", "gui.example.toml"),
        ("progress-only", "examples/gui/progress-only/theme.toml"),
        ("full-window", "examples/gui/full-window/theme.toml"),
    ] {
        let theme = Theme::load(&repo().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        compile(&theme, &work(tag)).unwrap_or_else(|e| panic!("{file}: {e}"));
    }
}

#[test]
fn reference_lists_every_string() {
    let text = std::fs::read_to_string(repo().join("gui.example.toml")).unwrap();
    for (key, default) in patcher_gui::strings::STRINGS {
        let line = format!("# {key} = {default:?}");
        assert!(text.contains(&line), "gui.example.toml lacks `{line}`");
    }
}
