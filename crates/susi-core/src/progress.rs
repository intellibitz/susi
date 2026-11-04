//! Per-call progress for a request whose caller asked to watch it (an MCP
//! `tools/call` carrying a `progressToken`). The host that runs the call
//! installs a sink on the running thread with [`with_sink`]; code deep in
//! the call reports human-readable progress lines with [`report`], which
//! does nothing when nobody is watching. A sink is a thread-scoped value:
//! work handed to another thread or process carries it explicitly
//! ([`current`], or a plane-bus stream), never implicitly.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Receives one progress line at a time.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Cooperative cancellation flag for an in-flight call (`mission_run`).
pub type Cancel = Arc<AtomicBool>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
    static CANCEL: RefCell<Option<Cancel>> = const { RefCell::new(None) };
}

/// Run `call` with `sink` receiving its progress lines on this thread. The
/// previous sink (usually none) is restored afterwards, also on panic.
pub fn with_sink<T>(sink: Option<Sink>, call: impl FnOnce() -> T) -> T {
    struct Restore(Option<Sink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            SINK.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = SINK.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), sink));
    let _restore = Restore(previous);
    call()
}

/// The sink watching this thread's call, to hand to work that runs
/// elsewhere.
#[must_use]
pub fn current() -> Option<Sink> {
    SINK.with(|slot| slot.borrow().clone())
}

/// Report one progress line to whoever watches this thread's call.
pub fn report(line: &str) {
    if let Some(sink) = current() {
        sink(line);
    }
}

/// Run `call` with `cancel` as this thread's cooperative cancellation flag.
/// The previous flag (usually none) is restored afterwards, also on panic.
pub fn with_cancel<T>(cancel: Option<Cancel>, call: impl FnOnce() -> T) -> T {
    struct Restore(Option<Cancel>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CANCEL.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = CANCEL.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), cancel));
    let _restore = Restore(previous);
    call()
}

/// The cancellation flag watching this thread's call, to hand to work that
/// runs elsewhere.
#[must_use]
pub fn current_cancel() -> Option<Cancel> {
    CANCEL.with(|slot| slot.borrow().clone())
}

/// Whether the caller has cancelled this thread's call.
#[must_use]
pub fn is_cancelled() -> bool {
    current_cancel().is_some_and(|cancel| cancel.load(Ordering::Acquire))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn collector() -> (Sink, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let seen = lines.clone();
        let sink: Sink = Arc::new(move |line: &str| {
            seen.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_string());
        });
        (sink, lines)
    }

    #[test]
    fn reports_reach_the_sink_only_while_it_is_installed() {
        report("nobody is watching");
        let (sink, lines) = collector();
        with_sink(Some(sink), || {
            report("one");
            report("two");
        });
        report("after");
        assert!(current().is_none());
        assert_eq!(
            *lines.lock().unwrap_or_else(|e| e.into_inner()),
            ["one", "two"]
        );
    }

    #[test]
    fn nested_sinks_restore_the_outer_one_and_threads_do_not_inherit() {
        let (outer, outer_lines) = collector();
        let (inner, inner_lines) = collector();
        with_sink(Some(outer), || {
            with_sink(Some(inner), || report("inner"));
            report("outer");
            let other = std::thread::spawn(|| current().is_none()).join();
            assert_eq!(other.ok(), Some(true));
            with_sink(None, || report("silenced"));
        });
        assert_eq!(
            *inner_lines.lock().unwrap_or_else(|e| e.into_inner()),
            ["inner"]
        );
        assert_eq!(
            *outer_lines.lock().unwrap_or_else(|e| e.into_inner()),
            ["outer"]
        );
    }

    #[test]
    fn a_panicking_call_still_restores_the_previous_sink() {
        let (sink, _) = collector();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_sink(Some(sink), || panic!("boom"))
        }));
        assert!(result.is_err());
        assert!(current().is_none());
    }

    #[test]
    fn cancel_flag_is_observed_and_restored_across_scopes_and_panics() {
        assert!(!is_cancelled());
        let outer = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(AtomicBool::new(true));
        with_cancel(Some(outer.clone()), || {
            assert!(!is_cancelled());
            with_cancel(Some(inner), || {
                assert!(is_cancelled());
            });
            assert!(!is_cancelled());
            outer.store(true, Ordering::Release);
            assert!(is_cancelled());
        });
        assert!(!is_cancelled());
        assert!(current_cancel().is_none());

        let flag = Arc::new(AtomicBool::new(true));
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_cancel(Some(flag), || panic!("boom"))
        }));
        assert!(panicked.is_err());
        assert!(current_cancel().is_none());
    }

    #[test]
    fn with_sink_none_and_with_cancel_none_leave_thread_locals_empty() {
        let out = with_sink(None, || {
            assert!(current().is_none());
            with_cancel(None, || {
                assert!(current_cancel().is_none());
                assert!(!is_cancelled());
                42
            })
        });
        assert_eq!(out, 42);
    }
}
