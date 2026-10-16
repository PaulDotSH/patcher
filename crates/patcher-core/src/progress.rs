//! Progress reporting hooks, implemented by the console and the GUI.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What a run of `begin` .. `end` measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Creator: hashing both trees (bytes).
    Hash,
    /// Creator: encoding changed files (bytes).
    Encode,
    /// Apply: checking the game's base files (bytes).
    Verify,
    /// Apply: decoding outputs into staging (bytes).
    Decode,
    /// Apply: moving files into place (operations).
    Commit,
    /// Revert: restoring files (operations).
    Revert,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Hash => "Hashing",
            Phase::Encode => "Encoding",
            Phase::Verify => "Verifying game files",
            Phase::Decode => "Patching",
            Phase::Commit => "Installing",
            Phase::Revert => "Reverting",
        }
    }

    /// Whether the phase counts bytes (otherwise operations).
    pub fn is_bytes(self) -> bool {
        !matches!(self, Phase::Commit | Phase::Revert)
    }
}

pub trait Progress: Sync {
    /// Starts a phase measured in `total` units (bytes or operations, see [`Phase`]).
    fn begin(&self, phase: Phase, total: u64);
    fn inc(&self, n: u64);
    fn end(&self);
    /// The file a worker starts on (called from several threads at once).
    fn file(&self, _path: &str) {}
    fn note(&self, _msg: &str) {}
}

pub struct Silent;

impl Progress for Silent {
    fn begin(&self, _: Phase, _: u64) {}
    fn inc(&self, _: u64) {}
    fn end(&self) {}
}

struct Current {
    phase: Phase,
    total: u64,
    started: Instant,
    drawn: Instant,
}

/// Single-line progress on stderr, redrawn at most every 100 ms.
pub struct Console {
    done: AtomicU64,
    phase: Mutex<Current>,
}

impl Default for Console {
    fn default() -> Self {
        let now = Instant::now();
        Console {
            done: AtomicU64::new(0),
            phase: Mutex::new(Current { phase: Phase::Verify, total: 0, started: now, drawn: now }),
        }
    }
}

impl Console {
    fn draw(&self, p: &Current, newline: bool) {
        let done = self.done.load(Ordering::Relaxed).min(p.total);
        let pct = if p.total == 0 { 100.0 } else { done as f64 * 100.0 / p.total as f64 };
        let amount = |n| if p.phase.is_bytes() { human_bytes(n) } else { n.to_string() };
        let mut err = std::io::stderr().lock();
        let _ = write!(
            err,
            "\r{}: {pct:5.1}%  {} / {}  [{:.1}s]   ",
            p.phase.label(),
            amount(done),
            amount(p.total),
            p.started.elapsed().as_secs_f64()
        );
        if newline {
            let _ = writeln!(err);
        }
        let _ = err.flush();
    }
}

impl Progress for Console {
    fn begin(&self, phase: Phase, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        let now = Instant::now();
        let mut p = self.phase.lock().unwrap();
        *p = Current { phase, total, started: now, drawn: now };
        self.draw(&p, false);
    }

    fn inc(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
        let Ok(mut p) = self.phase.try_lock() else { return };
        if p.drawn.elapsed() >= Duration::from_millis(100) {
            p.drawn = Instant::now();
            self.draw(&p, false);
        }
    }

    fn end(&self) {
        let p = self.phase.lock().unwrap();
        self.done.store(p.total, Ordering::Relaxed);
        self.draw(&p, true);
    }

    fn note(&self, msg: &str) {
        eprintln!("{msg}");
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.2} {}", UNITS[u]) }
}
