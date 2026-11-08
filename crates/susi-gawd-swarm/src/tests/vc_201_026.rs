use crate::cancel_propagate::{CancelBus, CancelToken, Descendant, WorkerKind};

#[test]
fn vc_201_026_cancels_cancellable_descendants_and_reports_irreversible() {
    let mut bus = CancelBus::default();
    bus.set_token(CancelToken::fresh("mission-1", CancelBus::now_unix() + 60));
    bus.register(Descendant {
        id: "local-1".into(),
        kind: WorkerKind::Local,
        cancellable: true,
        cancel_scope: None,
        signal: None,
    });
    bus.register(Descendant {
        id: "ext-1".into(),
        kind: WorkerKind::ExternalAgent,
        cancellable: true,
        cancel_scope: None,
        signal: None,
    });
    bus.register(Descendant {
        id: "peer-1".into(),
        kind: WorkerKind::PeerDispatch,
        cancellable: true,
        cancel_scope: None,
        signal: None,
    });
    bus.register(Descendant {
        id: "model-1".into(),
        kind: WorkerKind::ModelCall,
        cancellable: true,
        cancel_scope: None,
        signal: None,
    });
    bus.register(Descendant {
        id: "remote-irreversible".into(),
        kind: WorkerKind::PeerDispatch,
        cancellable: false,
        cancel_scope: None,
        signal: None,
    });
    let reports = bus.propagate();
    assert!(bus.terminated.contains("local-1"));
    assert!(bus.terminated.contains("model-1"));
    assert!(bus.irreversible_running.contains("remote-irreversible"));
    assert!(reports.iter().any(|r| r.contains("irreversible")));
    assert!(bus.token.as_ref().unwrap().cancelled);
}

#[test]
fn vc_201_026_deadline_remaining_propagates() {
    let tok = CancelToken::fresh("m", 100);
    assert_eq!(tok.remaining_secs(40), Some(60));
    assert_eq!(tok.remaining_secs(100), None);
    let mut tok2 = CancelToken::fresh("m", 100);
    tok2.cancel();
    assert_eq!(tok2.remaining_secs(40), None);
}

#[test]
fn vc_201_026_propagate_sets_cancellable_worker_signals_only() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let mut bus = CancelBus::default();
    bus.set_token(CancelToken::fresh(
        "mission-sig",
        CancelBus::now_unix() + 60,
    ));

    let cancellable_sig = Arc::new(AtomicBool::new(false));
    let irreversible_sig = Arc::new(AtomicBool::new(false));

    bus.register(Descendant {
        id: "worker-cancellable".into(),
        kind: WorkerKind::Local,
        cancellable: true,
        cancel_scope: None,
        signal: Some(Arc::clone(&cancellable_sig)),
    });
    bus.register(Descendant {
        id: "worker-irreversible".into(),
        kind: WorkerKind::PeerDispatch,
        cancellable: false,
        cancel_scope: None,
        signal: Some(Arc::clone(&irreversible_sig)),
    });

    let reports = bus.propagate();
    assert!(cancellable_sig.load(Ordering::Acquire));
    assert!(!irreversible_sig.load(Ordering::Acquire));
    assert!(bus.terminated.contains("worker-cancellable"));
    assert!(bus.irreversible_running.contains("worker-irreversible"));
    assert_eq!(reports.len(), 1);
}

#[test]
fn vc_201_026_propagate_without_token_is_noop_and_late_register_sets_signal() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let mut bus = CancelBus::default();
    assert!(bus.propagate().is_empty());
    assert!(bus.terminated.is_empty());

    let mut pre_cancelled = CancelToken::fresh("mission-late", CancelBus::now_unix() + 60);
    pre_cancelled.cancel();
    bus.set_token(pre_cancelled);

    let late_sig = Arc::new(AtomicBool::new(false));
    bus.register(Descendant {
        id: "late-worker".into(),
        kind: WorkerKind::ModelCall,
        cancellable: true,
        cancel_scope: None,
        signal: Some(Arc::clone(&late_sig)),
    });
    assert!(late_sig.load(Ordering::Acquire));
}

#[test]
fn vc_201_026_cancel_token_fresh_starts_uncancelled_with_matching_root_id() {
    let tok = CancelToken::fresh("mission-alpha", 500);
    assert_eq!(tok.root_id, "mission-alpha");
    assert_eq!(tok.deadline_unix, 500);
    assert!(!tok.cancelled);
}
