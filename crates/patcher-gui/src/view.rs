//! Writing the theme, the strings and the flow's state into the Slint globals.

use std::collections::HashMap;

use slint::{Brush, ModelRc, SharedString, VecModel};
use slint_interpreter::{ComponentInstance, Value};

use patcher_core::format::Meta;

use crate::flow::{BaseStatus, Config, Screen, State};
use crate::theme::{Background, Corner, Theme};

/// Strings drawn in the display font, uppercased when the theme asks for it.
const DISPLAY_KEYS: &[&str] = &[
    "next",
    "back",
    "patch",
    "patch_admin",
    "cancel",
    "close",
    "revert",
    "launch",
    "open_folder",
    "browse",
    "copy_details",
    "finishing",
    "welcome_heading",
    "license_heading",
    "folder_heading",
    "options_heading",
    "progress_heading",
    "progress_heading_revert",
    "done_heading",
    "error_heading",
];

/// Every string for this run: theme overrides, metadata filled in, display strings
/// uppercased when the theme asks for it.
pub fn strings(theme: &Theme, name: &str, version: &str, author: &str) -> HashMap<String, String> {
    crate::strings::STRINGS
        .iter()
        .map(|(k, _)| {
            let mut v = crate::strings::fill(theme.string(k), name, version, author);
            if theme.file.theme.uppercase && DISPLAY_KEYS.contains(k) {
                v = v.to_uppercase();
            }
            (k.to_string(), v)
        })
        .collect()
}

/// The flow's configuration for this theme and patch.
pub fn config(theme: &Theme, meta: &Meta) -> Config {
    let screens = &theme.file.screens;
    Config {
        welcome: screens.welcome,
        options: screens.options,
        license: theme.license.as_ref().map(|p| std::fs::read_to_string(p).unwrap_or_else(|e| format!("({}: {e})", p.display()))),
        require_accept: screens.require_accept,
        launch: theme.launch_program().is_some() || theme.file.launch.steam_app_id.is_some(),
        name: meta.name.clone(),
        version: meta.version.clone(),
        author: meta.author.clone(),
        description: meta.description.clone(),
        url: meta.url.clone(),
        strings: strings(theme, &meta.name, &meta.version, &meta.author),
    }
}

/// Font size (px) that fits the wordmark on one line in a window `width` wide, assuming a
/// condensed display face (about half an em per character).
pub fn wordmark_size(mark: &str, tag: &str, width: u32) -> f64 {
    let chars = (mark.chars().count() + tag.chars().count()) as f64;
    // Side padding, the gap before the tag and the tag's own padding.
    let avail = f64::from(width) - 88.0;
    (avail / (0.5 * chars + 0.75)).clamp(28.0, 60.0)
}

/// The wordmark and the word in the tag. The tag defaults to the last word.
pub fn wordmark(theme: &Theme, name: &str) -> (String, String) {
    let look = &theme.file.theme;
    let mut text = look.wordmark.clone().unwrap_or_else(|| name.to_string()).trim().to_string();
    if look.uppercase {
        text = text.to_uppercase();
    }
    match look.tag.as_deref() {
        Some("") => (text, String::new()),
        Some(tag) => {
            let tag = if look.uppercase { tag.to_uppercase() } else { tag.to_string() };
            match text.to_lowercase().strip_suffix(&tag.to_lowercase()) {
                Some(head) => (text[..head.len()].trim_end().to_string(), text[head.len()..].to_string()),
                None => (text, tag),
            }
        }
        None => match text.rsplit_once(' ') {
            Some((head, last)) => (head.trim_end().to_string(), last.to_string()),
            None => (String::new(), text),
        },
    }
}

fn set(inst: &ComponentInstance, global: &str, prop: &str, v: Value) {
    if let Err(e) = inst.set_global_property(global, prop, v) {
        // Only a mismatch between this file and api.slint can cause this.
        debug_assert!(false, "{global}.{prop}: {e}");
    }
}

fn s(v: &str) -> Value {
    Value::String(SharedString::from(v))
}

fn enum_value(ty: &str, v: &str) -> Value {
    Value::EnumerationValue(ty.into(), v.into())
}

/// Colours, fonts, images, strings and window title. Image files that fail to load are
/// left empty rather than failing the run.
pub fn apply_theme(inst: &ComponentInstance, theme: &Theme, strings: &HashMap<String, String>, name: &str) {
    for (token, c) in &theme.palette {
        let color = slint::Color::from_argb_encoded(c.to_argb());
        set(inst, "Theme", token, Value::Brush(Brush::SolidColor(color)));
    }
    set(inst, "Theme", "font-display", s(&theme.display.family));
    set(inst, "Theme", "font-body", s(&theme.body.family));
    set(inst, "Theme", "font-mono", s(&theme.mono.family));
    set(inst, "Theme", "shadows", Value::Bool(theme.file.theme.shadows));
    let bg = match theme.file.theme.background {
        Background::Glow => "glow",
        Background::Grid => "grid",
        Background::Image => "image",
        Background::None => "none",
    };
    set(inst, "Theme", "background", enum_value("BackgroundKind", bg));
    let corner = match theme.file.credit.position {
        Corner::BottomLeft => "bottom-left",
        Corner::BottomRight => "bottom-right",
        Corner::TopLeft => "top-left",
        Corner::TopRight => "top-right",
    };
    set(inst, "Theme", "credit-position", enum_value("Corner", corner));
    let image = |p: &Option<std::path::PathBuf>| p.as_ref().and_then(|p| slint::Image::load_from_path(p).ok());
    if let Some(img) = image(&theme.background_image) {
        set(inst, "Theme", "background-image", Value::Image(img));
    }
    if let Some(img) = image(&theme.icon) {
        set(inst, "Theme", "window-icon", Value::Image(img));
    }
    let logo = image(&theme.logo);
    set(inst, "Theme", "has-logo", Value::Bool(logo.is_some()));
    if let Some(img) = logo {
        set(inst, "Theme", "logo", Value::Image(img));
    }
    let (mark, tag) = wordmark(theme, name);
    set(inst, "Theme", "wordmark-size", Value::Number(wordmark_size(&mark, &tag, theme.file.window.width)));
    set(inst, "Theme", "wordmark", s(&mark));
    set(inst, "Theme", "tag", s(&tag));
    for (k, v) in strings {
        set(inst, "Strings", &k.replace('_', "-"), s(v));
    }
    let title = theme.file.window.title.clone().unwrap_or_else(|| name.to_string());
    let _ = inst.set_property("window-title", s(&title));
}

pub fn apply_state(inst: &ComponentInstance, st: &State) {
    let screen = match st.screen {
        Screen::Welcome => "welcome",
        Screen::License => "license",
        Screen::Folder => "folder",
        Screen::Options => "options",
        Screen::Progress => "progress",
        Screen::Done => "done",
        Screen::Error => "error",
    };
    let status = match st.base_status {
        BaseStatus::Unknown => "unknown",
        BaseStatus::Checking => "checking",
        BaseStatus::Ok => "ok",
        BaseStatus::AlreadyApplied => "already-applied",
        BaseStatus::Mismatch => "mismatch",
        BaseStatus::NotFound => "not-found",
    };
    set(inst, "Patcher", "screen", enum_value("Screen", screen));
    set(inst, "Patcher", "base-status", enum_value("BaseStatus", status));
    for (k, v) in [
        ("mod-name", &st.mod_name),
        ("mod-version", &st.mod_version),
        ("mod-description", &st.mod_description),
        ("mod-url", &st.mod_url),
        ("byline", &st.byline),
        ("license-text", &st.license_text),
        ("base-detail", &st.base_detail),
        ("folder-error", &st.folder_error),
        ("result", &st.result),
        ("error-text", &st.error_text),
        ("error-details", &st.error_details),
        ("next-label", &st.next_label),
    ] {
        set(inst, "Patcher", k, s(v));
    }
    // Only pushed when it changed, so typing in the field is not interrupted.
    if inst.get_global_property("Patcher", "game-path").ok() != Some(s(&st.game_path)) {
        set(inst, "Patcher", "game-path", s(&st.game_path));
    }
    for (k, v) in [
        ("options-merged", st.options_merged),
        ("backup", st.backup),
        ("can-revert", st.can_revert),
        ("reverting", st.reverting),
        ("require-accept", st.require_accept),
        ("license-accepted", st.license_accepted),
        ("can-browse", st.can_browse),
        ("can-cancel", st.can_cancel),
        ("finishing", st.finishing),
        ("can-launch", st.can_launch),
        ("can-back", st.can_back),
        ("can-next", st.can_next),
        ("can-close", st.can_close),
    ] {
        set(inst, "Patcher", k, Value::Bool(v));
    }
}

/// Live numbers of the running job.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Live {
    pub progress: f64,
    pub phase: String,
    pub current_file: String,
    pub speed: String,
    pub eta: String,
    pub log: Vec<String>,
}

pub fn apply_live(inst: &ComponentInstance, live: &Live) {
    set(inst, "Patcher", "progress", Value::Number(live.progress));
    set(inst, "Patcher", "phase", s(&live.phase));
    set(inst, "Patcher", "current-file", s(&live.current_file));
    set(inst, "Patcher", "speed", s(&live.speed));
    set(inst, "Patcher", "eta", s(&live.eta));
    let log: Vec<Value> = live.log.iter().map(|l| s(l)).collect();
    set(inst, "Patcher", "log", Value::Model(ModelRc::new(VecModel::from(log))));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(wordmark: Option<&str>, tag: Option<&str>) -> Theme {
        let mut t = Theme::builtin();
        t.file.theme.wordmark = wordmark.map(String::from);
        t.file.theme.tag = tag.map(String::from);
        t
    }

    #[test]
    fn wordmark_splits() {
        let w = |m, t, name| wordmark(&theme(m, t), name);
        assert_eq!(w(None, None, "patcher"), ("".into(), "PATCHER".into()));
        assert_eq!(w(None, None, "Cool Mod Remix"), ("COOL MOD".into(), "REMIX".into()));
        assert_eq!(w(Some("Cool Mod"), Some("mod"), "x"), ("COOL".into(), "MOD".into()));
        assert_eq!(w(Some("Cool Mod"), Some("Remix"), "x"), ("COOL MOD".into(), "REMIX".into()));
        assert_eq!(w(Some("Cool Mod"), Some(""), "x"), ("COOL MOD".into(), "".into()));
    }
}
