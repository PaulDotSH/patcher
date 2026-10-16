//! The theme file (`--gui theme.toml`): parsing, validation and resolved values.
//!
//! The same loader runs in the creator (on the author's folder) and in the stub (on the
//! extracted bundle), so a theme that loads at create time loads identically at run time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::color::{self, Color};
use crate::strings;

#[derive(Debug, thiserror::Error)]
#[error("{file}: {msg}")]
pub struct ThemeError {
    pub file: String,
    pub msg: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Base {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    #[default]
    Glow,
    Grid,
    Image,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    #[default]
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeFile {
    pub window: Window,
    pub theme: Look,
    pub layout: Layout,
    pub screens: Screens,
    pub paths: OsPaths,
    pub launch: Launch,
    pub credit: Credit,
    pub strings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Window {
    pub width: u32,
    pub height: u32,
    pub title: Option<String>,
    pub icon: Option<String>,
}

impl Default for Window {
    fn default() -> Self {
        Window { width: 760, height: 480, title: None, icon: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Look {
    pub base: Base,
    pub accent: Option<String>,
    pub wordmark: Option<String>,
    pub tag: Option<String>,
    pub logo: Option<String>,
    pub background: Background,
    pub background_image: Option<String>,
    pub shadows: bool,
    pub colors: BTreeMap<String, String>,
    pub fonts: Fonts,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            base: Base::Dark,
            accent: None,
            wordmark: None,
            tag: None,
            logo: None,
            background: Background::Glow,
            background_image: None,
            shadows: true,
            colors: BTreeMap::new(),
            fonts: Fonts::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Fonts {
    pub display: Option<String>,
    pub body: Option<String>,
    pub mono: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Layout {
    pub installer: Option<String>,
    pub parts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Screens {
    pub welcome: bool,
    pub options: bool,
    pub license: Option<String>,
    pub require_accept: bool,
}

impl Default for Screens {
    fn default() -> Self {
        Screens { welcome: true, options: true, license: None, require_accept: false }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct OsPaths {
    pub windows: Vec<String>,
    pub linux: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Launch {
    pub windows: Option<String>,
    pub linux: Option<String>,
    pub steam_app_id: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Credit {
    pub position: Corner,
}

/// Parts of the built-in window an author can replace, with the component each file must
/// export.
pub const PARTS: [(&str, &str); 10] = [
    ("titlebar", "TitleBar"),
    ("background", "Background"),
    ("welcome", "WelcomeScreen"),
    ("license", "LicenseScreen"),
    ("folder", "FolderScreen"),
    ("options", "OptionsScreen"),
    ("progress", "ProgressScreen"),
    ("done", "DoneScreen"),
    ("error", "ErrorScreen"),
    ("buttons", "NavButtons"),
];

/// A font the theme uses: the author's file (absolute) or a built-in one, and the family
/// name `.slint` code refers to it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontChoice {
    pub file: Option<PathBuf>,
    pub family: String,
}

/// A loaded, validated theme. Paths are absolute.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Folder of the theme file; everything it references lives inside it.
    pub dir: PathBuf,
    /// The theme file itself.
    pub path: PathBuf,
    pub file: ThemeFile,
    pub palette: BTreeMap<&'static str, Color>,
    pub display: FontChoice,
    pub body: FontChoice,
    pub mono: FontChoice,
    pub icon: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    pub background_image: Option<PathBuf>,
    pub license: Option<PathBuf>,
    pub installer: Option<PathBuf>,
    /// Part key -> author file.
    pub parts: BTreeMap<String, PathBuf>,
}

pub const BUILTIN_DISPLAY: &str = "Anton";
pub const BUILTIN_BODY: &str = "Inter";
pub const BUILTIN_MONO: &str = "JetBrains Mono";

impl Theme {
    /// The built-in default theme.
    pub fn builtin() -> Theme {
        Theme::from_file(ThemeFile::default(), PathBuf::new(), PathBuf::new()).expect("default theme is valid")
    }

    pub fn load(path: &Path) -> Result<Theme, ThemeError> {
        let err = |msg: String| ThemeError { file: path.display().to_string(), msg };
        let text = std::fs::read_to_string(path).map_err(|e| err(e.to_string()))?;
        let file: ThemeFile = toml::from_str(&text).map_err(|e| err(e.to_string().trim_end().to_string()))?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let dir = std::fs::canonicalize(if dir.as_os_str().is_empty() { Path::new(".") } else { &dir })
            .map_err(|e| err(e.to_string()))?;
        Theme::from_file(file, dir, path.to_path_buf()).map_err(err)
    }

    fn from_file(file: ThemeFile, dir: PathBuf, path: PathBuf) -> Result<Theme, String> {
        let w = &file.window;
        if !(320..=4096).contains(&w.width) || !(240..=4096).contains(&w.height) {
            return Err(format!("[window] size {}x{} is outside 320x240 to 4096x4096", w.width, w.height));
        }
        let look = &file.theme;
        let accent = match &look.accent {
            Some(s) => Some(Color::parse(s).ok_or_else(|| format!("[theme] accent: {s:?} is not a #rrggbb colour"))?),
            None => None,
        };
        let mut overrides = BTreeMap::new();
        for (k, v) in &look.colors {
            if !color::TOKENS.contains(&k.as_str()) {
                return Err(format!("[theme.colors] unknown colour {k:?}; known: {}", color::TOKENS.join(", ")));
            }
            let c = Color::parse(v).ok_or_else(|| format!("[theme.colors] {k}: {v:?} is not a #rrggbb colour"))?;
            overrides.insert(k.clone(), c);
        }
        let palette = color::derive(look.base, accent, &overrides);
        for k in file.strings.keys() {
            if !strings::is_key(k) {
                return Err(format!("[strings] unknown string {k:?} (see docs/gui.md for the list)"));
            }
        }
        if look.background == Background::Image && look.background_image.is_none() {
            return Err("[theme] background = \"image\" needs background_image".into());
        }

        let asset = |key: &str, rel: &Option<String>| -> Result<Option<PathBuf>, String> {
            rel.as_deref().map(|r| inside(&dir, key, r)).transpose()
        };
        let font = |key: &str, rel: &Option<String>, builtin: &str| -> Result<FontChoice, String> {
            match asset(key, rel)? {
                Some(p) => Ok(FontChoice { family: font_family(&p).map_err(|e| format!("{key}: {e}"))?, file: Some(p) }),
                None => Ok(FontChoice { file: None, family: builtin.into() }),
            }
        };
        let mut parts = BTreeMap::new();
        for (k, v) in &file.layout.parts {
            if !PARTS.iter().any(|(p, _)| p == k) {
                let known: Vec<_> = PARTS.iter().map(|p| p.0).collect();
                return Err(format!("[layout.parts] unknown part {k:?}; known: {}", known.join(", ")));
            }
            parts.insert(k.clone(), slint_file(&format!("[layout.parts] {k}"), inside(&dir, k, v)?)?);
        }
        let installer = asset("[layout] installer", &file.layout.installer)?
            .map(|p| slint_file("[layout] installer", p))
            .transpose()?;
        Ok(Theme {
            display: font("[theme.fonts] display", &look.fonts.display, BUILTIN_DISPLAY)?,
            body: font("[theme.fonts] body", &look.fonts.body, BUILTIN_BODY)?,
            mono: font("[theme.fonts] mono", &look.fonts.mono, BUILTIN_MONO)?,
            icon: asset("[window] icon", &w.icon)?,
            logo: asset("[theme] logo", &look.logo)?,
            background_image: asset("[theme] background_image", &look.background_image)?,
            license: asset("[screens] license", &file.screens.license)?,
            installer,
            parts,
            palette,
            dir,
            path,
            file,
        })
    }

    /// A string by key, the theme's override or the default.
    pub fn string(&self, key: &str) -> &str {
        self.file.strings.get(key).map(String::as_str).unwrap_or_else(|| {
            strings::STRINGS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap_or("")
        })
    }

    /// Default game-folder candidates for this OS, unexpanded.
    pub fn default_paths(&self) -> &[String] {
        if cfg!(windows) { &self.file.paths.windows } else { &self.file.paths.linux }
    }

    /// Program to launch for this OS, relative to the game folder.
    pub fn launch_program(&self) -> Option<&str> {
        if cfg!(windows) { self.file.launch.windows.as_deref() } else { self.file.launch.linux.as_deref() }
    }
}

/// Resolves `rel` against `dir`, requiring an existing file inside `dir`.
fn inside(dir: &Path, key: &str, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::Prefix(_))) {
        return Err(format!("{key}: {rel:?} must be a path inside the theme folder"));
    }
    let full = dir.join(p);
    if !full.is_file() {
        return Err(format!("{key}: {} does not exist", full.display()));
    }
    Ok(full)
}

fn slint_file(key: &str, p: PathBuf) -> Result<PathBuf, String> {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("slint")) {
        Ok(p)
    } else {
        Err(format!("{key}: {} is not a .slint file", p.display()))
    }
}

/// The family name `.slint` code uses for a TTF/OTF file.
pub fn font_family(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let face = ttf_parser::Face::parse(&data, 0).map_err(|e| format!("{}: not a font ({e})", path.display()))?;
    let name = |id| {
        face.names().into_iter().filter(|n| n.name_id == id).find_map(|n| n.to_string())
    };
    name(ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
        .or_else(|| name(ttf_parser::name_id::FAMILY))
        .ok_or_else(|| format!("{}: font has no family name", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_dir(tag: &str, toml: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("patcher-gui-theme-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        std::fs::write(dir.join("theme.toml"), toml).unwrap();
        dir
    }

    fn load(tag: &str, toml: &str, files: &[&str]) -> Result<Theme, String> {
        Theme::load(&theme_dir(tag, toml, files).join("theme.toml")).map_err(|e| e.msg)
    }

    #[test]
    fn empty_theme_is_the_default() {
        let t = load("empty", "", &[]).unwrap();
        assert_eq!((t.file.window.width, t.file.window.height), (760, 480));
        assert_eq!(t.palette["accent"], color::DEFAULT_ACCENT);
        assert_eq!(t.display.family, "Anton");
        assert_eq!(t.string("patch"), "Patch");
        assert!(t.file.theme.shadows && t.file.screens.welcome);
    }

    #[test]
    fn values_and_assets_resolve() {
        let t = load(
            "full",
            r##"
            [window]
            width = 900
            icon = "img/icon.png"
            [theme]
            accent = "#ff3366"
            background = "image"
            background_image = "img/bg.png"
            [theme.colors]
            bad = "#ff0000"
            [layout.parts]
            progress = "ui/progress.slint"
            [strings]
            patch = "Install"
            "##,
            &["img/icon.png", "img/bg.png", "ui/progress.slint"],
        )
        .unwrap();
        assert_eq!(t.file.window.width, 900);
        assert_eq!(t.palette["accent"], Color::rgb(0xff3366));
        assert_eq!(t.palette["bad"], Color::rgb(0xff0000));
        assert!(t.icon.as_ref().unwrap().ends_with("img/icon.png"));
        assert!(t.parts["progress"].ends_with("ui/progress.slint"));
        assert_eq!(t.string("patch"), "Install");
        assert_eq!(t.string("next"), "Next");
    }

    #[test]
    fn mistakes_are_reported() {
        let cases: &[(&str, &str, &str)] = &[
            ("unknown-key", "[theme]\nacent = \"#fff\"", "acent"),
            ("bad-color", "[theme]\naccent = \"lime\"", "accent"),
            ("bad-token", "[theme.colors]\nbackground = \"#000\"", "unknown colour"),
            ("bad-string", "[strings]\npatchh = \"x\"", "patchh"),
            ("missing-file", "[window]\nicon = \"nope.png\"", "does not exist"),
            ("escape", "[theme]\nlogo = \"../x.png\"", "inside the theme folder"),
            ("bad-part", "[layout.parts]\nprogres = \"p.slint\"", "unknown part"),
            ("not-slint", "[layout]\ninstaller = \"a.txt\"", "not a .slint"),
            ("no-image", "[theme]\nbackground = \"image\"", "background_image"),
            ("too-small", "[window]\nwidth = 100", "outside"),
        ];
        for (tag, toml, needle) in cases {
            let e = load(tag, toml, &["a.txt", "p.slint"]).unwrap_err();
            assert!(e.contains(needle), "{tag}: {e}");
        }
    }
}
