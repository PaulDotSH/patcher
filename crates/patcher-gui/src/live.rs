//! Progress reported by worker threads, read by the UI timer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use patcher_core::progress::{human_bytes, Phase, Progress};

use crate::view::Live;

/// Relative cost per unit of each phase: hashing a byte is cheaper than decoding one, and
/// the commit (renames) gets a small fixed share.
fn weight(phase: Phase, total: u64, all_bytes: u64) -> f64 {
    match phase {
        Phase::Verify => total as f64 * 0.4,
        Phase::Decode => total as f64,
        Phase::Commit => (all_bytes as f64 * 0.04).max(1.0),
        _ => (total as f64).max(1.0),
    }
}

#[derive(Default)]
struct Inner {
    totals: Vec<(Phase, u64)>,
    phase: Option<Phase>,
    phase_total: u64,
    file: String,
    log: Vec<String>,
    /// (time, overall progress, bytes) samples for speed and ETA.
    samples: VecDeque<(Instant, f64, u64)>,
    bytes_before: u64,
}

#[derive(Default)]
pub struct LiveProgress {
    done: AtomicU64,
    inner: Mutex<Inner>,
}

impl LiveProgress {
    pub fn reset(&self, totals: Vec<(Phase, u64)>) {
        self.done.store(0, Ordering::Relaxed);
        *self.inner.lock().unwrap() = Inner { totals, ..Default::default() };
    }

    /// The phase running now, for the flow (commit disables cancel).
    pub fn phase(&self) -> Option<Phase> {
        self.inner.lock().unwrap().phase
    }

    pub fn log(&self) -> Vec<String> {
        self.inner.lock().unwrap().log.clone()
    }

    pub fn snapshot(&self) -> Live {
        let mut g = self.inner.lock().unwrap();
        let done = self.done.load(Ordering::Relaxed).min(g.phase_total);
        let all_bytes: u64 = g.totals.iter().filter(|(p, _)| p.is_bytes()).map(|t| t.1).sum();
        let total_weight: f64 = g.totals.iter().map(|&(p, t)| weight(p, t, all_bytes)).sum();
        let mut before = 0.0;
        let mut progress = 0.0;
        if let Some(phase) = g.phase {
            for &(p, t) in &g.totals {
                if p == phase {
                    let frac = if t == 0 { 1.0 } else { done as f64 / t as f64 };
                    progress = (before + frac * weight(p, t, all_bytes)) / total_weight.max(1.0);
                    break;
                }
                before += weight(p, t, all_bytes);
            }
        }
        let bytes = g.bytes_before + if g.phase.is_some_and(Phase::is_bytes) { done } else { 0 };
        let now = Instant::now();
        g.samples.push_back((now, progress, bytes));
        while g.samples.len() > 2 && now - g.samples[0].0 > Duration::from_secs(2) {
            g.samples.pop_front();
        }
        let (t0, p0, b0) = g.samples[0];
        let dt = (now - t0).as_secs_f64();
        let (speed, eta) = if dt > 0.4 {
            let rate = (progress - p0) / dt;
            let eta = if rate > 1e-6 && progress < 1.0 {
                let s = ((1.0 - progress) / rate).round() as u64;
                format!("{}:{:02}", s / 60, s % 60)
            } else {
                String::new()
            };
            let bps = (bytes.saturating_sub(b0)) as f64 / dt;
            (if bps > 0.0 { format!("{}/s", human_bytes(bps as u64)) } else { String::new() }, eta)
        } else {
            (String::new(), String::new())
        };
        Live {
            progress: progress.clamp(0.0, 1.0),
            phase: g.phase.map(|p| p.label().to_string()).unwrap_or_default(),
            current_file: g.file.clone(),
            speed,
            eta,
            log: g.log.clone(),
        }
    }
}

impl Progress for LiveProgress {
    fn begin(&self, phase: Phase, total: u64) {
        let mut g = self.inner.lock().unwrap();
        if g.phase.is_some_and(Phase::is_bytes) {
            g.bytes_before += g.phase_total;
        }
        if !g.totals.iter().any(|t| t.0 == phase) {
            g.totals.push((phase, total));
        }
        g.phase = Some(phase);
        g.phase_total = total;
        g.file.clear();
        self.done.store(0, Ordering::Relaxed);
    }

    fn inc(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    fn end(&self) {
        let g = self.inner.lock().unwrap();
        self.done.store(g.phase_total, Ordering::Relaxed);
    }

    fn file(&self, path: &str) {
        if let Ok(mut g) = self.inner.try_lock() {
            g.file.clear();
            g.file.push_str(path);
        }
    }

    fn note(&self, msg: &str) {
        self.inner.lock().unwrap().log.push(msg.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_weighted_and_monotonic() {
        let p = LiveProgress::default();
        p.reset(vec![(Phase::Verify, 1000), (Phase::Decode, 1000), (Phase::Commit, 10)]);
        let mut last = 0.0;
        for phase in [Phase::Verify, Phase::Decode, Phase::Commit] {
            let total = if phase == Phase::Commit { 10 } else { 1000 };
            p.begin(phase, total);
            for _ in 0..10 {
                p.inc(total / 10);
                let now = p.snapshot().progress;
                assert!(now >= last, "{phase:?}: {now} < {last}");
                last = now;
            }
            p.end();
        }
        assert!((last - 1.0).abs() < 1e-9);
        // Verify is weighted lighter than decode.
        p.reset(vec![(Phase::Verify, 1000), (Phase::Decode, 1000), (Phase::Commit, 10)]);
        p.begin(Phase::Verify, 1000);
        p.inc(1000);
        let after_verify = p.snapshot().progress;
        assert!(after_verify > 0.2 && after_verify < 0.35, "{after_verify}");
    }
}
