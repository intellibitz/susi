//! Boot-phase timing for the composition roots and the daemon loop.
//!
//! A boot that takes minutes instead of seconds is invisible otherwise: the
//! CLI composition runs before tracing is initialised and the daemon's
//! stderr is a null device. Every phase is timed; with `SUSI_BOOT_TRACE`
//! set each one is printed to stderr as it finishes (offset since the first
//! traced phase, then the phase's own duration), and the first time a phase
//! runs slower than [`SLOW_PHASE`] it is reported through tracing.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A phase this slow is reported even without `SUSI_BOOT_TRACE`.
pub const SLOW_PHASE: Duration = Duration::from_secs(5);

fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

fn enabled() -> bool {
    std::env::var_os("SUSI_BOOT_TRACE").is_some()
}

/// Run `f` as the boot phase `label` and report how long it took.
pub fn phase<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = f();
    finished(label, started);
    out
}

/// Report the boot phase `label` that began at `started` — for phases that
/// cannot be wrapped in a closure (an `.await` in an async composition).
pub fn finished(label: &str, started: Instant) {
    let since = origin();
    let took = started.elapsed();
    if enabled() {
        eprintln!(
            "[boot-trace] +{:>9.3}s {:>9.3}s  {label}",
            since.elapsed().as_secs_f64(),
            took.as_secs_f64()
        );
    }
    if took >= SLOW_PHASE && first_slow(label) {
        tracing::warn!(
            "[boot] phase `{label}` took {:.1}s (SUSI_BOOT_TRACE=1 prints every phase)",
            took.as_secs_f64()
        );
    }
}

/// A slow phase is reported once per process: the daemon's rediscovery
/// loop re-runs the bootstrap phases every pass, and the boot is what the
/// warning is for.
fn first_slow(label: &str) -> bool {
    static SEEN: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(label.to_string()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::phase;

    #[test]
    fn phase_returns_the_closure_value() {
        assert_eq!(phase("unit", || 41 + 1), 42);
    }

    #[test]
    fn a_slow_phase_is_reported_once_per_label() {
        assert!(super::first_slow("unit: slow"));
        assert!(!super::first_slow("unit: slow"));
        assert!(super::first_slow("unit: other"));
    }

    #[test]
    fn phase_preserves_complex_return_types_and_invokes_once() {
        let mut count = 0usize;
        let out: Result<&'static str, &'static str> = phase("unit: result", || {
            count += 1;
            Ok("booted")
        });
        assert_eq!(out, Ok("booted"));
        assert_eq!(count, 1);
    }

    #[test]
    fn finished_completes_for_fast_and_backdated_slow_instants() {
        let now = super::Instant::now();
        super::finished("unit: fast-finished", now);
        if let Some(backdated) = now.checked_sub(super::SLOW_PHASE + super::Duration::from_secs(1))
        {
            super::finished("unit: backdated-slow-finished", backdated);
            assert!(!super::first_slow("unit: backdated-slow-finished"));
        }
    }

    #[test]
    fn slow_phase_constant_is_five_seconds_and_origin_is_monotonic() {
        assert_eq!(super::SLOW_PHASE, super::Duration::from_secs(5));
        let t0 = super::origin();
        let t1 = super::origin();
        assert_eq!(t0, t1);
        assert!(t0.elapsed() >= super::Duration::ZERO);
    }
}
