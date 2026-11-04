//! Optional host callbacks so swarm never depends on `susi-gawd`.
//!
//! Distillation audit (`ReflexTrainer`) stays in the host crate; AMAS kicks it
//! off through this seam.

use crate::susi_error::EaiResult;
use std::path::Path;
use std::sync::OnceLock;

pub trait HostHooks: Send + Sync {
    fn audit_distillation_state(&self, workspace: &Path) -> EaiResult<String>;
}

static HOOKS: OnceLock<Box<dyn HostHooks>> = OnceLock::new();

pub fn init(hooks: Box<dyn HostHooks>) {
    let _ = HOOKS.set(hooks);
}

struct NoOpHostHooks;
impl HostHooks for NoOpHostHooks {
    fn audit_distillation_state(&self, _workspace: &Path) -> EaiResult<String> {
        // Nothing was audited: say so rather than report a healthy state.
        Err(crate::susi_error::EaiError::internal(
            "[HOST_UNWIRED] distillation audit unavailable: host hooks were never initialized",
        ))
    }
}

pub(crate) fn hooks() -> &'static dyn HostHooks {
    HOOKS.get_or_init(|| Box::new(NoOpHostHooks)).as_ref()
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    struct FakeHostHooks {
        tag: &'static str,
    }

    impl HostHooks for FakeHostHooks {
        fn audit_distillation_state(&self, workspace: &Path) -> EaiResult<String> {
            Ok(format!("{}:{}", self.tag, workspace.display()))
        }
    }

    #[test]
    fn noop_host_hooks_returns_host_unwired_error_with_expected_marker() {
        let noop = NoOpHostHooks;
        let err = noop
            .audit_distillation_state(Path::new("/tmp/ws"))
            .expect_err("noop must fail");
        assert!(err.to_string().contains("[HOST_UNWIRED]"));
    }

    #[test]
    fn custom_host_hooks_trait_object_executes_audit_distillation_state() {
        let hook: Box<dyn HostHooks> = Box::new(FakeHostHooks { tag: "audited" });
        let out = hook
            .audit_distillation_state(Path::new("workspace-a"))
            .expect("custom hook ok");
        assert_eq!(out, "audited:workspace-a");
    }

    #[test]
    fn init_and_hooks_accessor_preserve_first_initialized_hook_instance() {
        init(Box::new(FakeHostHooks { tag: "primary" }));
        init(Box::new(FakeHostHooks { tag: "secondary" }));
        let res = hooks().audit_distillation_state(Path::new("ws"));
        if let Ok(msg) = res {
            assert!(msg.starts_with("primary:"));
        }
    }

    #[test]
    fn hooks_accessor_is_deterministic_across_repeated_invocations() {
        let a = hooks().audit_distillation_state(Path::new("same-ws"));
        let b = hooks().audit_distillation_state(Path::new("same-ws"));
        assert_eq!(a.is_ok(), b.is_ok());
    }

    #[test]
    fn noop_host_hooks_includes_distillation_audit_unavailable_context() {
        let noop = NoOpHostHooks;
        let err = noop
            .audit_distillation_state(Path::new(""))
            .expect_err("noop must report unwired error");
        let msg = err.to_string();
        assert!(msg.contains("distillation audit unavailable"));
        assert!(msg.contains("host hooks were never initialized"));
    }
}
