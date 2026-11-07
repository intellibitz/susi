//! One lifecycle contract for local runtimes (VC-201-042).

use crate::runtime_lifecycle::{
    Capabilities, InferOutcome, Runtime, RuntimeBackend, RuntimeStatus,
};
use crate::susi_error::{EaiError, EaiResult};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

/// Scriptable fake backend: healthy/unhealthy answers and infer outcomes
/// are queued so tests exercise the contract deterministically.
struct Fake {
    discovered: bool,
    health_answers: RefCell<VecDeque<Result<(), String>>>,
    infer_answers: RefCell<VecDeque<InferOutcome>>,
    loaded: Cell<bool>,
    unloaded: Cell<bool>,
}

impl Fake {
    fn new(discovered: bool) -> Self {
        Self {
            discovered,
            health_answers: RefCell::new(VecDeque::new()),
            infer_answers: RefCell::new(VecDeque::new()),
            loaded: Cell::new(false),
            unloaded: Cell::new(false),
        }
    }
    fn push_health(&self, a: Result<(), String>) {
        self.health_answers.borrow_mut().push_back(a);
    }
    fn push_infer(&self, o: InferOutcome) {
        self.infer_answers.borrow_mut().push_back(o);
    }
}

impl RuntimeBackend for Fake {
    fn discover(&self) -> EaiResult<bool> {
        Ok(self.discovered)
    }
    fn load(&self, _model: &str) -> EaiResult<()> {
        self.loaded.set(true);
        Ok(())
    }
    fn health(&self) -> EaiResult<Result<(), String>> {
        Ok(self
            .health_answers
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err("no probe answer scripted".into())))
    }
    fn infer(&self, _prompt: &str) -> EaiResult<InferOutcome> {
        self.infer_answers
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| EaiError::config("no infer answer scripted"))
    }
    fn cancel(&self) -> EaiResult<()> {
        Ok(())
    }
    fn unload(&self) -> EaiResult<()> {
        self.unloaded.set(true);
        Ok(())
    }
}

fn caps(cancel: bool, unload: bool) -> Capabilities {
    Capabilities {
        can_cancel: cancel,
        can_unload: unload,
    }
}

#[test]
fn vc_201_042_process_presence_does_not_establish_readiness() {
    let fake = Fake::new(true);
    // Loaded but health probe says sick → not ready, no infer.
    fake.push_health(Err("gpu wedged".into()));
    let mut rt = Runtime::new("llamacpp", caps(true, true), &fake);
    rt.discover().unwrap();
    rt.load("m.gguf").unwrap();
    assert!(!rt.ready().unwrap(), "probe failure is not readiness");
    assert!(matches!(rt.status(), RuntimeStatus::Unhealthy { .. }));
    let err = rt.infer("hi").unwrap_err().to_string();
    assert!(err.contains("not ready"), "{err}");
}

#[test]
fn vc_201_042_full_verb_chain_native_runtime() {
    let fake = Fake::new(true);
    fake.push_health(Ok(()));
    fake.push_infer(InferOutcome::Done {
        output: "ok".into(),
    });
    let mut rt = Runtime::new("llamacpp", caps(true, true), &fake);
    rt.discover().unwrap();
    rt.load("m.gguf").unwrap();
    assert!(rt.ready().unwrap());
    match rt.infer("hi").unwrap() {
        InferOutcome::Done { output } => assert_eq!(output, "ok"),
        other => panic!("expected Done, got {other:?}"),
    }
    rt.cancel().unwrap();
    rt.unload().unwrap();
    assert!(matches!(rt.status(), RuntimeStatus::Discovered));
    assert!(fake.unloaded.get());
}

#[test]
fn vc_201_042_unsupported_ops_return_typed_errors() {
    // HTTP-managed runtimes may not support cancel/unload.
    let fake = Fake::new(true);
    let mut rt = Runtime::new("remote", caps(false, false), &fake);
    rt.discover().unwrap();
    let e = rt.cancel().unwrap_err().to_string();
    assert!(e.contains("does not support"), "{e}");
    let e = rt.unload().unwrap_err().to_string();
    assert!(e.contains("does not support"), "{e}");
}

#[test]
fn vc_201_042_gone_runtime_refuses_load() {
    let fake = Fake::new(false);
    let mut rt = Runtime::new("vllm", caps(true, true), &fake);
    rt.discover().unwrap();
    assert!(matches!(rt.status(), RuntimeStatus::Gone));
    assert!(rt.load("m").is_err());
}

#[test]
fn vc_201_042_health_recheck_can_restore_readiness() {
    let fake = Fake::new(true);
    fake.push_health(Err("boot".into()));
    fake.push_health(Ok(()));
    fake.push_infer(InferOutcome::Done { output: "a".into() });
    let mut rt = Runtime::new("sglang", caps(true, true), &fake);
    rt.discover().unwrap();
    rt.load("m").unwrap();
    assert!(!rt.ready().unwrap());
    assert!(rt.ready().unwrap(), "second probe recovers");
    assert!(rt.infer("x").is_ok());
}

#[test]
fn lifecycle_contract_covers_all_seven_operations() {
    let fake = Fake::new(true);
    fake.push_health(Ok(()));
    fake.push_health(Ok(()));
    fake.push_infer(InferOutcome::Cancelled);
    let mut rt = Runtime::new("contract", caps(true, true), &fake);

    rt.discover().unwrap();
    rt.load("model.gguf").unwrap();
    // The health verb is a first-class op, not only ready()'s innards.
    assert!(matches!(rt.health().unwrap(), Ok(())));
    assert!(rt.ready().unwrap());
    assert!(matches!(
        rt.infer("prompt").unwrap(),
        InferOutcome::Cancelled
    ));
    rt.cancel().unwrap();
    rt.unload().unwrap();
    assert!(matches!(rt.status(), RuntimeStatus::Discovered));
}

#[test]
fn vc_201_042_health_verb_marks_and_recovers() {
    // A failed probe on a discovered runtime marks Unhealthy; a later
    // healthy answer does not manufacture Ready without a loaded model.
    let fake = Fake::new(true);
    fake.push_health(Err("daemon wedged".into()));
    let mut rt = Runtime::new("ollama", caps(false, false), &fake);
    rt.discover().unwrap();
    assert!(rt.health().unwrap().is_err());
    assert!(matches!(rt.status(), RuntimeStatus::Unhealthy { .. }));
    assert!(!rt.ready().unwrap(), "no model staged — never Ready");
    // A Gone runtime answering health re-proves presence (Discovered).
    let up = Fake::new(false);
    up.push_health(Ok(()));
    let mut gone = Runtime::new("docker-ollama", caps(false, false), &up);
    gone.discover().unwrap();
    assert!(matches!(gone.status(), RuntimeStatus::Gone));
    assert!(matches!(gone.health().unwrap(), Ok(())));
    assert!(matches!(gone.status(), RuntimeStatus::Discovered));
}

#[test]
fn vc_201_042_production_scan_drives_the_contract() {
    // The ecosystem scan must run its runtimes through the contract, so a
    // host daemon's `running` flag is a health-probe answer (VC-201-042),
    // not a bare port check the contract never saw.
    let src = include_str!("../local_ecosystem.rs");
    assert!(
        src.contains("runtime_lifecycle::") && src.contains("Runtime::new"),
        "local_ecosystem must drive runtimes through runtime_lifecycle::Runtime"
    );
    assert!(
        src.contains("rt.discover()") && src.contains("rt.health()"),
        "the scan must drive discover and health through the contract"
    );
}
