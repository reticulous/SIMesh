//! Rate-limited progress reporting for the long phases of server-side
//! work (`pack build`, `optimize`).
//!
//! These runs go for tens of minutes. Without a heartbeat the only way to
//! tell a working process from a hung one is to watch CPU counters from
//! another shell, which is exactly the state this module exists to end.
//! Output goes to stderr so it never contaminates piped results.

/// Emit a progress line at most this often (seconds). Candidate sweeps on a
/// region pack run for tens of minutes; without this the only way to tell a
/// long run from a hung one is to watch CPU counters from another shell.
const PROGRESS_INTERVAL_S: f64 = 15.0;

/// A phase faster than this says nothing at all — no interim line, no
/// summary. Progress output is for runs long enough to look hung.
const FINISH_THRESHOLD_S: f64 = 3.0;

/// Rate-limited progress reporter, safe to poke from every rayon worker.
pub struct Progress {
    label: &'static str,
    total: usize,
    done: std::sync::atomic::AtomicUsize,
    last: std::sync::Mutex<std::time::Instant>,
    start: std::time::Instant,
    enabled: bool,
    /// Whether any interim line was emitted, so `finish` knows if the phase
    /// left the user waiting.
    printed: std::sync::atomic::AtomicBool,
}

impl Progress {
    pub fn new(label: &'static str, total: usize, enabled: bool) -> Self {
        let now = std::time::Instant::now();
        Self {
            label,
            total,
            done: std::sync::atomic::AtomicUsize::new(0),
            last: std::sync::Mutex::new(now),
            start: now,
            enabled,
            printed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Count one unit of work; print if the interval has elapsed. The
    /// mutex is only taken on the rare printing path.
    pub fn tick(&self) {
        use std::sync::atomic::Ordering;
        let n = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        if !self.enabled || self.total == 0 {
            return;
        }
        let now = std::time::Instant::now();
        let mut last = match self.last.try_lock() {
            Ok(l) => l,
            Err(_) => return, // another worker is already printing
        };
        // Only the interval gates a line. Completion is NOT special-cased:
        // `finish` reports that, and forcing a "100%" tick here made every
        // sub-second phase emit two redundant lines.
        if now.duration_since(*last).as_secs_f64() < PROGRESS_INTERVAL_S {
            return;
        }
        self.printed.store(true, Ordering::Relaxed);
        *last = now;
        let elapsed = now.duration_since(self.start).as_secs_f64();
        let frac = n as f64 / self.total as f64;
        let eta = if frac > 0.0 { elapsed / frac - elapsed } else { 0.0 };
        eprintln!(
            "  {}: {}/{} ({:.0}%) — {:.0}s elapsed, ~{:.0}s left",
            self.label, n, self.total, frac * 100.0, elapsed, eta
        );
    }

    /// Report the phase total, but only when it was slow enough to have
    /// been worth watching — a phase that finishes in a fraction of a second
    /// should say nothing at all.
    pub fn finish(&self) {
        use std::sync::atomic::Ordering;
        if !self.enabled || self.total == 0 {
            return;
        }
        let elapsed = self.start.elapsed().as_secs_f64();
        if elapsed < FINISH_THRESHOLD_S && !self.printed.load(Ordering::Relaxed) {
            return;
        }
        eprintln!("  {}: {} done in {:.1}s", self.label, self.total, elapsed);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn disabled_progress_still_counts() {
        let p = Progress::new("x", 10, false);
        for _ in 0..10 {
            p.tick();
        }
        p.finish(); // must not panic and must print nothing
        assert_eq!(p.done.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn zero_total_is_safe() {
        // An empty phase (no files, no tiles) must not divide by zero.
        let p = Progress::new("empty", 0, true);
        p.tick();
        p.finish();
    }

    #[test]
    fn counting_is_exact_under_concurrency() {
        use std::sync::Arc;
        let p = Arc::new(Progress::new("threads", 1000, false));
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let p = Arc::clone(&p);
                std::thread::spawn(move || {
                    for _ in 0..125 {
                        p.tick();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(p.done.load(Ordering::Relaxed), 1000);
    }
}
