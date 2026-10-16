//! Physical memory detection and a byte budget that bounds concurrent work.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::{Condvar, Mutex};

/// Total physical memory in bytes (8 GiB if it cannot be determined).
pub fn total_memory() -> u64 {
    imp().unwrap_or(8 << 30)
}

#[cfg(windows)]
fn imp() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: MEMORYSTATUSEX is plain data; all-zero is a valid value.
    let mut s: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    s.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `s` is a properly sized, writable MEMORYSTATUSEX.
    (unsafe { GlobalMemoryStatusEx(&mut s) } != 0).then_some(s.ullTotalPhys)
}

#[cfg(not(windows))]
fn imp() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Counting semaphore over bytes. A request larger than the whole budget waits until it
/// can run alone, so oversized jobs still make progress. A thread that already holds a
/// permit never waits (the budget may go negative): with rayon's work stealing it can be
/// running a nested job while its own permit is held, and blocking there could deadlock.
pub struct Budget {
    total: i64,
    free: Mutex<i64>,
    cv: Condvar,
}

/// Must be dropped on the thread that acquired it.
pub struct Permit<'a> {
    budget: &'a Budget,
    n: i64,
    _not_send: PhantomData<*const ()>,
}

thread_local! {
    static HELD: Cell<u32> = const { Cell::new(0) };
}

impl Budget {
    pub fn new(total: u64) -> Self {
        let total = total.clamp(1, 1 << 60) as i64;
        Budget { total, free: Mutex::new(total), cv: Condvar::new() }
    }

    pub fn total(&self) -> u64 {
        self.total as u64
    }

    pub fn acquire(&self, n: u64) -> Permit<'_> {
        let n = n.min(self.total as u64) as i64;
        let mut free = self.free.lock().unwrap();
        if HELD.get() == 0 {
            while *free < n {
                free = self.cv.wait(free).unwrap();
            }
        }
        *free -= n;
        HELD.set(HELD.get() + 1);
        Permit { budget: self, n, _not_send: PhantomData }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        HELD.set(HELD.get() - 1);
        *self.budget.free.lock().unwrap() += self.n;
        self.budget.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_acquire_never_blocks() {
        let b = Budget::new(100);
        let outer = b.acquire(100);
        // Before, this waited forever for `outer`, which this thread holds.
        let inner = b.acquire(100);
        drop(inner);
        drop(outer);
        let again = b.acquire(100);
        drop(again);
        assert_eq!(*b.free.lock().unwrap(), 100);
    }
}
