//! Progress reporting hooks (console now, a GUI can implement the same trait later).

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub trait Progress: Sync {
    /// Starts a phase measured in `total` units (bytes).
    fn begin(&self, label: &str, total: u64);
    fn inc(&self, n: u64);
    fn end(&self);
    fn note(&self, _msg: &str) {}
}

pub struct Silent;

impl Progress for Silent {
    fn begin(&self, _: &str, _: u64) {}
    fn inc(&self, _: u64) {}
    fn end(&self) {}
}

struct Phase {
    label: String,
    total: u64,
    started: Instant,
    drawn: Instant,
}

/// Single-line progress on stderr, redrawn at most every 100 ms.
pub struct Console {
    done: AtomicU64,
    phase: Mutex<Phase>,
}

impl Default for Console {
    fn default() -> Self {
        let now = Instant::now();
        Console {
            done: AtomicU64::new(0),
            phase: Mutex::new(Phase { label: String::new(), total: 0, started: now, drawn: now }),
        }
    }
}

impl Console {
    fn draw(&self, p: &Phase, newline: bool) {
        let done = self.done.load(Ordering::Relaxed).min(p.total);
        let pct = if p.total == 0 { 100.0 } else { done as f64 * 100.0 / p.total as f64 };
        let mut err = std::io::stderr().lock();
        let _ = write!(
            err,
            "\r{}: {pct:5.1}%  {} / {}  [{:.1}s]   ",
            p.label,
            human_bytes(done),
            human_bytes(p.total),
            p.started.elapsed().as_secs_f64()
        );
        if newline {
            let _ = writeln!(err);
        }
        let _ = err.flush();
    }
}

impl Progress for Console {
    fn begin(&self, label: &str, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        let now = Instant::now();
        let mut p = self.phase.lock().unwrap();
        *p = Phase { label: label.to_string(), total, started: now, drawn: now };
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
