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
        Ok("Reflex substrate optimal (host hooks unwired).".into())
    }
}

pub(crate) fn hooks() -> &'static dyn HostHooks {
    HOOKS.get_or_init(|| Box::new(NoOpHostHooks)).as_ref()
}
