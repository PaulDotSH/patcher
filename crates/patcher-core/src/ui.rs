//! Console helpers shared by the stub and the creator CLI's `apply` command.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::format::Index;
use crate::progress::human_bytes;
use crate::{paths, Patch};

pub fn banner(patch: &Patch) -> String {
    let m = &patch.index.meta;
    let mut s = String::new();
    let title = match (m.name.is_empty(), m.version.is_empty()) {
        (true, _) => "Unnamed mod".to_string(),
        (false, true) => m.name.clone(),
        (false, false) => format!("{} {}", m.name, m.version),
    };
    s.push_str(&title);
    if !m.author.is_empty() {
        s.push_str(&format!(" by {}", m.author));
    }
    s.push('\n');
    if !m.description.is_empty() {
        s.push_str(&format!("{}\n", m.description));
    }
    if !m.url.is_empty() {
        s.push_str(&format!("{}\n", m.url));
    }
    let idx = &patch.index;
    s.push_str(&format!(
        "{} files to write ({}), {} to delete, patch size {}{}\n",
        idx.writes.len(),
        human_bytes(idx.writes.iter().map(|w| w.check.size).sum()),
        idx.deletes.len(),
        human_bytes(patch.header.total_len),
        if idx.strong_hashes { ", BLAKE3-verified" } else { "" }
    ));
    s
}

/// True if `dir` holds the files this patch expects (checked by path only).
pub fn looks_like_game(idx: &Index, dir: &Path) -> bool {
    let probe: Vec<_> = idx.base_files.iter().take(16).collect();
    !probe.is_empty() && probe.iter().all(|b| matches!(paths::resolve_existing(dir, &b.path), Ok(Some(_))))
}

/// First candidate directory that looks like the game.
pub fn find_game_dir(idx: &Index, candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|d| looks_like_game(idx, d)).cloned()
}

pub fn prompt(question: &str) -> Option<String> {
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(line.trim().trim_matches('"').to_string())
}

pub fn confirm(question: &str) -> bool {
    prompt(&format!("{question} [y/N] ")).is_some_and(|a| matches!(a.to_lowercase().as_str(), "y" | "yes"))
}
