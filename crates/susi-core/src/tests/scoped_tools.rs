use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize};

struct Probe {
    tools: Vec<ScopedTool>,
    marker: String,
    closed: AtomicBool,
    calls: AtomicUsize,
}

impl ScopedToolProvider for Probe {
    fn tools(&self) -> &[ScopedTool] {
        &self.tools
    }
    fn check(&self) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            Err("owner closed".into())
        } else {
            Ok(())
        }
    }
    fn call(
        &self,
        _: &str,
        args: &Value,
        _: &Path,
        _: Option<&str>,
        cancel: Option<Cancel>,
        turn_cancel: Cancel,
    ) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if args["separate"] == true {
            assert!(!Arc::ptr_eq(cancel.as_ref().unwrap(), &turn_cancel));
            turn_cancel.store(true, Ordering::Release);
        }
        if args["cancel"] == true {
            cancel.unwrap().store(true, Ordering::Release);
        }
        Ok(self.marker.clone())
    }
}

fn probe(marker: &str) -> (Arc<Probe>, Arc<ToolScope>, Cancel) {
    let provider = Arc::new(Probe {
        tools: vec![ScopedTool {
            name: "probe".into(),
            description: marker.into(),
            input_schema: json!({"type":"object"}),
            required_actions: vec![],
        }],
        marker: marker.into(),
        closed: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let scope = ToolScope::new(provider.clone(), cancel.clone());
    (provider, scope, cancel)
}

#[test]
fn scoped_tools_restore_nested_bindings_and_do_not_implicitly_cross_threads() {
    let (_, first, _) = probe("first");
    let (_, second, _) = probe("second");
    assert!(current().is_none());
    with_scope(Some(first.clone()), || {
        assert!(std::thread::spawn(|| current().is_none()).join().unwrap());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_scope(Some(second), || panic!("restore on unwind"));
        }));
        assert!(result.is_err());
        assert!(Arc::ptr_eq(&current().unwrap(), &first));
    });
    assert!(current().is_none());
}

#[test]
fn scoped_tools_parallel_workers_keep_their_owner_and_clear_it_afterward() {
    let (_, first, _) = probe("first");
    let (_, second, _) = probe("second");
    std::thread::scope(|threads| {
        for (scope, marker) in [(first, "first"), (second, "second")] {
            threads.spawn(move || {
                let output = with_scope(Some(scope), || {
                    crate::parallel::map((0..16).collect(), |_| {
                        current()
                            .unwrap()
                            .call("probe", &json!({}), Path::new("."), None)
                            .unwrap()
                    })
                });
                assert_eq!(output, vec![marker; 16]);
                assert!(current().is_none());
            });
        }
    });
}

#[test]
fn scoped_tools_refuse_closed_unknown_and_parent_or_child_cancelled_calls() {
    let (provider, scope, parent) = probe("ok");
    assert!(scope
        .call("missing", &json!({}), Path::new("."), None)
        .is_err());
    let child = Arc::new(AtomicBool::new(true));
    crate::progress::with_cancel(Some(child.clone()), || {
        assert!(scope
            .call("probe", &json!({}), Path::new("."), None)
            .is_err());
    });
    child.store(false, Ordering::Release);
    parent.store(true, Ordering::Release);
    crate::progress::with_cancel(Some(child.clone()), || {
        assert!(scope
            .call("probe", &json!({}), Path::new("."), None)
            .is_err());
    });
    parent.store(false, Ordering::Release);
    provider.closed.store(true, Ordering::Release);
    assert!(scope
        .call("probe", &json!({}), Path::new("."), None)
        .is_err());
    assert_eq!(provider.calls.load(Ordering::Acquire), 0);
    provider.closed.store(false, Ordering::Release);
    // A cancellation that arrives during the call suppresses the late result.
    assert!(scope
        .call("probe", &json!({"cancel":true}), Path::new("."), None)
        .is_err());
    assert_eq!(provider.calls.load(Ordering::Acquire), 1);
}

#[test]
fn scoped_tools_pass_both_tokens_to_blocking_owner() {
    let (_, scope, parent) = probe("ok");
    let child = Arc::new(AtomicBool::new(false));
    crate::progress::with_cancel(Some(child.clone()), || {
        assert!(scope
            .call("probe", &json!({"separate":true}), Path::new("."), None)
            .is_err());
    });
    assert!(parent.load(Ordering::Acquire));
    assert!(!child.load(Ordering::Acquire));
}

#[test]
fn scoped_tools_parallel_workers_keep_child_cancellation_separate_from_turn() {
    let (provider, scope, parent) = probe("ok");
    let child = Arc::new(AtomicBool::new(false));
    with_scope(Some(scope), || {
        crate::progress::with_cancel(Some(child.clone()), || {
            // Cancelling through the handed-off child token suppresses the
            // provider result, without cancelling the wider session turn.
            let results = crate::parallel::map((0..16).collect(), |_| {
                assert!(Arc::ptr_eq(
                    &crate::progress::current_cancel().unwrap(),
                    &child
                ));
                current()
                    .unwrap()
                    .call("probe", &json!({"cancel":true}), Path::new("."), None)
            });
            assert!(results.iter().all(Result::is_err));
            assert!(child.load(Ordering::Acquire));
            assert!(!parent.load(Ordering::Acquire));
            let calls = provider.calls.load(Ordering::Acquire);
            assert!(calls > 0);
            // The original turn still binds when its child remains live.
            child.store(false, Ordering::Release);
            parent.store(true, Ordering::Release);
            let results = crate::parallel::map((0..16).collect(), |_| {
                assert!(Arc::ptr_eq(
                    &crate::progress::current_cancel().unwrap(),
                    &child
                ));
                current()
                    .unwrap()
                    .call("probe", &json!({}), Path::new("."), None)
            });
            assert!(results.iter().all(Result::is_err));
            assert!(!child.load(Ordering::Acquire));
            assert_eq!(provider.calls.load(Ordering::Acquire), calls);
        })
    });
    assert!(crate::progress::current_cancel().is_none());
}

#[test]
fn scoped_tools_parallel_handoff_leaves_unscoped_worker_behavior_unchanged() {
    if crate::parallel::threads() < 2 {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(true));
    crate::progress::with_cancel(Some(cancel), || {
        let inherited = crate::parallel::map((0..16).collect(), |_| {
            crate::progress::current_cancel().is_some()
        });
        assert_eq!(inherited, vec![false; 16]);
        assert!(crate::progress::is_cancelled());
    });
}
