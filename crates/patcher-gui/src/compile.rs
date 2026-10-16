//! Turning a theme into a compiled Slint component.
//!
//! The built-in library is written to `<work>/patcher/`, next to two generated files:
//! `parts.slint` (each part from the built-in library or the author's file) and
//! `main.slint` (fonts, the installer, the credit, and the window). The creator runs this
//! to reject broken themes; the stub runs it to build its window. Both see the same
//! sources, so a theme that compiles at create time compiles for the player.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use slint_interpreter::{Compiler, ComponentDefinition, DiagnosticLevel};

use crate::strings::STRINGS;
use crate::theme::{canonical, Theme, PARTS};

const SOURCES: &[(&str, &str)] = &[
    ("api.slint", include_str!("../ui/api.slint")),
    ("widgets.slint", include_str!("../ui/widgets.slint")),
    ("lib.slint", include_str!("../ui/lib.slint")),
    ("shell.slint", include_str!("../ui/shell.slint")),
    ("credit.slint", include_str!("../ui/credit.slint")),
    ("titlebar.slint", include_str!("../ui/titlebar.slint")),
    ("background.slint", include_str!("../ui/background.slint")),
    ("welcome.slint", include_str!("../ui/welcome.slint")),
    ("license.slint", include_str!("../ui/license.slint")),
    ("folder.slint", include_str!("../ui/folder.slint")),
    ("options.slint", include_str!("../ui/options.slint")),
    ("progress.slint", include_str!("../ui/progress.slint")),
    ("done.slint", include_str!("../ui/done.slint")),
    ("error.slint", include_str!("../ui/error.slint")),
    ("buttons.slint", include_str!("../ui/buttons.slint")),
];

/// Built-in fonts, imported by every theme, and their licenses.
const FONTS: &[(&str, &[u8])] = &[
    ("anton.ttf", include_bytes!("../fonts/anton.ttf")),
    ("inter-400.ttf", include_bytes!("../fonts/inter-400.ttf")),
    ("inter-600.ttf", include_bytes!("../fonts/inter-600.ttf")),
    ("jetbrains-mono-500.ttf", include_bytes!("../fonts/jetbrains-mono-500.ttf")),
    ("OFL-anton.txt", include_bytes!("../fonts/OFL-anton.txt")),
    ("OFL-inter.txt", include_bytes!("../fonts/OFL-inter.txt")),
    ("OFL-jetbrainsmono.txt", include_bytes!("../fonts/OFL-jetbrainsmono.txt")),
];

/// Built-in file for each part key.
fn builtin_part_file(key: &str) -> &'static str {
    match key {
        "titlebar" => "titlebar.slint",
        "background" => "background.slint",
        "welcome" => "welcome.slint",
        "license" => "license.slint",
        "folder" => "folder.slint",
        "options" => "options.slint",
        "progress" => "progress.slint",
        "done" => "done.slint",
        "error" => "error.slint",
        "buttons" => "buttons.slint",
        _ => unreachable!("unknown part {key}"),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the theme's Slint code does not compile:\n  {}", .0.join("\n  "))]
    Slint(Vec<String>),
}

/// A path as a `.slint` string literal.
fn lit(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    format!("\"{}\"", s.replace('"', "\\\""))
}

fn slint_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// The library's `api.slint` with one `Strings` property per string key.
fn api_source() -> String {
    let props: String = STRINGS
        .iter()
        .map(|(k, v)| format!("    in property <string> {}: {};\n", k.replace('_', "-"), slint_string(v)))
        .collect();
    SOURCES[0].1.replace("    // STRINGS\n", &props)
}

/// Writes the built-in library and the generated entry files into `work`; returns the
/// path of `main.slint`.
pub fn write_sources(theme: &Theme, work: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(work)?;
    let work = canonical(work)?;
    let lib = work.join("patcher");
    std::fs::create_dir_all(lib.join("fonts"))?;
    for (name, src) in SOURCES {
        let text = if *name == "api.slint" { api_source() } else { (*src).to_string() };
        std::fs::write(lib.join(name), text)?;
    }
    for (name, data) in FONTS {
        std::fs::write(lib.join("fonts").join(name), data)?;
    }

    let mut parts = String::from("// Generated: each part from the built-in library or the theme.\n");
    for (key, component) in PARTS {
        let file = match theme.parts.get(key) {
            Some(p) => canonical(p)?,
            None => lib.join(builtin_part_file(key)),
        };
        parts += &format!("export {{ {component} }} from {};\n", lit(&file));
    }
    std::fs::write(work.join("parts.slint"), parts)?;

    let mut main = String::from("// Generated: fonts, the installer and the window.\n");
    for (name, _) in FONTS.iter().filter(|(n, _)| n.ends_with(".ttf")) {
        main += &format!("import {};\n", lit(&lib.join("fonts").join(name)));
    }
    for font in [&theme.display, &theme.body, &theme.mono] {
        if let Some(f) = &font.file {
            main += &format!("import {};\n", lit(&canonical(f)?));
        }
    }
    let installer = match &theme.installer {
        Some(p) => canonical(p)?,
        None => lib.join("shell.slint"),
    };
    main += &format!(
        r#"import {{ Installer }} from {installer};
import {{ Credit }} from {credit};
import {{ Patcher, Theme, Strings }} from "@patcher";
export {{ Patcher, Theme, Strings }}

export component Root inherits Window {{
    in property <string> window-title;
    title: root.window-title;
    no-frame: true;
    width: {w}px;
    height: {h}px;
    icon: Theme.window-icon;
    background: Theme.bg;
    default-font-family: Theme.font-body;
    default-font-size: 14px;
    Installer {{
        width: 100%;
        height: 100%;
    }}
    Credit {{
        width: 100%;
        height: 100%;
    }}
}}
"#,
        installer = lit(&installer),
        credit = lit(&lib.join("credit.slint")),
        w = theme.file.window.width,
        h = theme.file.window.height,
    );
    let main_path = work.join("main.slint");
    std::fs::write(&main_path, main)?;
    Ok(main_path)
}

/// Writes the sources into `work` and compiles them. Errors list `file:line:col: message`.
pub fn compile(theme: &Theme, work: &Path) -> Result<ComponentDefinition, CompileError> {
    let main = write_sources(theme, work)?;
    let work = main.parent().unwrap();
    let mut compiler = Compiler::default();
    compiler.set_library_paths(HashMap::from([
        ("patcher".to_string(), work.join("patcher").join("lib.slint")),
        ("parts".to_string(), work.join("parts.slint")),
    ]));
    let result = spin_on::spin_on(compiler.build_from_path(&main));
    let errors: Vec<String> = result
        .diagnostics()
        .filter(|d| d.level() == DiagnosticLevel::Error)
        .map(|d| {
            let (line, col) = d.line_column();
            let file = d.source_file().map(|f| f.display().to_string()).unwrap_or_default();
            format!("{file}:{line}:{col}: {}", d.message())
        })
        .collect();
    match result.component("Root") {
        Some(def) if errors.is_empty() => Ok(def),
        _ => Err(CompileError::Slint(if errors.is_empty() { vec!["no Root component".into()] } else { errors })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("patcher-gui-compile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn theme_with(tag: &str, toml: &str, files: &[(&str, &str)]) -> Theme {
        let dir = work(&format!("{tag}-theme"));
        for (p, text) in files {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("theme.toml"), toml).unwrap();
        Theme::load(&dir.join("theme.toml")).unwrap()
    }

    #[test]
    fn builtin_theme_compiles() {
        let def = compile(&Theme::builtin(), &work("builtin")).unwrap();
        let globals: Vec<_> = def.globals().collect();
        for g in ["Patcher", "Theme", "Strings"] {
            assert!(globals.iter().any(|n| n == g), "{g} not exported: {globals:?}");
        }
        let strings: Vec<_> = def.global_properties("Strings").unwrap().map(|(n, _)| n).collect();
        assert_eq!(strings.len(), STRINGS.len());
    }

    #[test]
    fn a_replaced_part_compiles() {
        let t = theme_with(
            "part",
            "[layout.parts]\nprogress = \"ui/progress.slint\"\n",
            &[(
                "ui/progress.slint",
                r#"import { Patcher, Theme, ProgressBar, Heading } from "@patcher";
export component ProgressScreen inherits Rectangle {
    VerticalLayout {
        padding: 40px;
        Heading { text: "Working"; }
        ProgressBar { value: Patcher.progress; }
    }
}
"#,
            )],
        );
        compile(&t, &work("part")).unwrap();
    }

    #[test]
    fn a_replaced_installer_compiles() {
        let t = theme_with(
            "installer",
            "[layout]\ninstaller = \"main.slint\"\n",
            &[(
                "main.slint",
                r#"import { Patcher, Strings, Button, TitleBar } from "@patcher";
export component Installer inherits Rectangle {
    background: #223;
    TitleBar { y: 0; }
    Button { text: Strings.next; clicked => { Patcher.next(); } }
}
"#,
            )],
        );
        compile(&t, &work("installer")).unwrap();
    }

    #[test]
    fn errors_name_the_author_file() {
        let t = theme_with(
            "broken",
            "[layout.parts]\ndone = \"done.slint\"\n",
            &[("done.slint", "export component DoneScreen inherits Rectangle {\n    nonsense: 3px;\n}\n")],
        );
        let Err(CompileError::Slint(errs)) = compile(&t, &work("broken")) else { panic!("compiled") };
        assert!(errs.iter().any(|e| e.contains("done.slint:2")), "{errs:?}");
    }
}
