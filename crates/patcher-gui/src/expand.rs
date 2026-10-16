//! Expanding `~` and environment variables in default game-folder candidates.

use std::path::PathBuf;

/// Expands `s` for the current OS. `None` when a variable it names is not set, so the
/// candidate is skipped rather than turned into a wrong path.
pub fn expand(s: &str) -> Option<PathBuf> {
    expand_with(s, cfg!(windows), |k| std::env::var(k).ok())
}

/// `~` or `~/...` becomes the home folder. On Windows `%VAR%` is expanded, elsewhere
/// `$VAR` and `${VAR}`.
pub fn expand_with(s: &str, windows: bool, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let mut out = String::new();
    let mut rest = s;
    if rest == "~" || rest.starts_with("~/") || rest.starts_with("~\\") {
        out.push_str(&env(if windows { "USERPROFILE" } else { "HOME" })?);
        rest = &rest[1..];
    }
    while let Some(c) = rest.chars().next() {
        if windows && c == '%' {
            let end = rest[1..].find('%')? + 1;
            out.push_str(&env(&rest[1..end])?);
            rest = &rest[end + 1..];
        } else if !windows && c == '$' {
            let (name, next) = if let Some(braced) = rest.strip_prefix("${") {
                let end = braced.find('}')?;
                (&braced[..end], &braced[end + 1..])
            } else {
                let body = &rest[1..];
                let end = body.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(body.len());
                (&body[..end], &body[end..])
            };
            if name.is_empty() {
                return None;
            }
            out.push_str(&env(name)?);
            rest = next;
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    Some(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(k: &str) -> Option<String> {
        match k {
            "HOME" => Some("/home/p".into()),
            "USERPROFILE" => Some(r"C:\Users\p".into()),
            "ProgramFiles(x86)" => Some(r"C:\Program Files (x86)".into()),
            "XDG_DATA_HOME" => Some("/home/p/.local/share".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_each_form() {
        let w = |s| expand_with(s, true, env).map(|p| p.to_string_lossy().into_owned());
        let l = |s| expand_with(s, false, env).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(w(r"%ProgramFiles(x86)%\Steam\common\Game").as_deref(), Some(r"C:\Program Files (x86)\Steam\common\Game"));
        assert_eq!(w(r"~\Games").as_deref(), Some(r"C:\Users\p\Games"));
        assert_eq!(l("~/.steam/common/Game").as_deref(), Some("/home/p/.steam/common/Game"));
        assert_eq!(l("$HOME/x").as_deref(), Some("/home/p/x"));
        assert_eq!(l("${XDG_DATA_HOME}/Steam").as_deref(), Some("/home/p/.local/share/Steam"));
        assert_eq!(l("/opt/game").as_deref(), Some("/opt/game"));
        assert_eq!(w(r"D:\Games\%").as_deref(), None, "unterminated");
        assert_eq!(l("$NOPE/x"), None, "unset variable");
        assert_eq!(w("%NOPE%"), None);
        assert_eq!(l("~user/x").as_deref(), Some("~user/x"), "only a bare ~ is the home folder");
    }
}
