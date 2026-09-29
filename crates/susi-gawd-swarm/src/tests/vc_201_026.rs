use crate::cancel_propagate::{CancelBus, CancelToken, Descendant, WorkerKind};

#[test]
fn vc_201_026_cancels_cancellable_descendants_and_reports_irreversible() {
    let mut bus = CancelBus::default();
    bus.set_token(CancelToken::fresh("mission-1", CancelBus::now_unix() + 60));
    bus.register(Descendant {
        id: "local-1".into(),
        kind: WorkerKind::Local,
        cancellable: true,
    });
    bus.register(Descendant {
        id: "ext-1".into(),
        kind: WorkerKind::ExternalAgent,
        cancellable: true,
    });
    bus.register(Descendant {
        id: "peer-1".into(),
        kind: WorkerKind::PeerDispatch,
        cancellable: true,
    });
    bus.register(Descendant {
        id: "model-1".into(),
        kind: WorkerKind::ModelCall,
        cancellable: true,
    });
    bus.register(Descendant {
        id: "remote-irreversible".into(),
        kind: WorkerKind::PeerDispatch,
        cancellable: false,
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
