//! One lifecycle contract for local runtimes (VC-201-042).
//!
//! Every supported runtime — native process or HTTP-managed — answers the
//! same verbs: discover, load, ready, infer, cancel, unload, health.
//! Unsupported operations return typed errors, never silent success, and
//! readiness comes only from an explicit health probe: a live process
//! handle or an open port alone never counts as ready.

use crate::susi_error::{eai_bail as bail, EaiError, EaiResult};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Discover,
    Load,
    Ready,
    Infer,
    Cancel,
    Unload,
    Health,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeStatus {
    /// Known to exist; nothing loaded.
    Discovered,
    /// Model artifacts staged into the runtime.
    Loaded {
        model: String,
    },
    /// Health probe confirmed inference is possible.
    Ready {
        model: String,
    },
    /// Present but health probe fails — NOT usable for inference.
    Unhealthy {
        reason: String,
    },
    Gone,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InferOutcome {
    Done { output: String },
    Cancelled,
    Unavailable { reason: String },
}

/// Runtime behavior surface the lifecycle contract drives. Each kind of
/// runtime supplies its own; the contract state-machine is shared.
pub trait RuntimeBackend {
    fn discover(&self) -> EaiResult<bool>;
    fn load(&self, model: &str) -> EaiResult<()>;
    /// Explicit readiness probe — process presence is NOT sufficient.
    fn health(&self) -> EaiResult<Result<(), String>>;
    fn infer(&self, prompt: &str) -> EaiResult<InferOutcome>;
    fn cancel(&self) -> EaiResult<()>;
    fn unload(&self) -> EaiResult<()>;
}

/// Which verbs a runtime class actually supports. Unsupported verbs
/// produce `LifecycleError::Unsupported` instead of a silent no-op.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub can_cancel: bool,
    pub can_unload: bool,
}

pub struct Runtime<'a> {
    pub id: String,
    caps: Capabilities,
    backend: &'a dyn RuntimeBackend,
    status: RuntimeStatus,
    /// Last loaded model — Unhealthy drops the status' copy, but an
    /// unhealthy runtime must stay re-probeable back to Ready.
    loaded_model: Option<String>,
    running: BTreeMap<String, String>,
}

impl<'a> Runtime<'a> {
    pub fn new(id: &str, caps: Capabilities, backend: &'a dyn RuntimeBackend) -> Self {
        Self {
            id: id.to_string(),
            caps,
            backend,
            status: RuntimeStatus::Gone,
            loaded_model: None,
            running: BTreeMap::new(),
        }
    }

    pub fn status(&self) -> &RuntimeStatus {
        &self.status
    }

    pub fn discover(&mut self) -> EaiResult<()> {
        self.status = if self.backend.discover()? {
            RuntimeStatus::Discovered
        } else {
            RuntimeStatus::Gone
        };
        Ok(())
    }

    pub fn load(&mut self, model: &str) -> EaiResult<()> {
        if self.status == RuntimeStatus::Gone {
            bail!("runtime {} is gone; discover again", self.id);
        }
        self.backend.load(model)?;
        self.loaded_model = Some(model.to_string());
        self.status = RuntimeStatus::Loaded {
            model: model.to_string(),
        };
        Ok(())
    }

    /// The contract's `Health` verb: run the readiness probe directly,
    /// without demanding a staged model the way `ready()` does. Health
    /// answers — never assumed state — decide: a failed probe on a runtime
    /// that exists marks it `Unhealthy`, and a healthy answer on a `Gone`
    /// runtime re-proves presence (`Discovered`). It never manufactures
    /// `Ready`: inference still requires a loaded model via `ready()`.
    pub fn health(&mut self) -> EaiResult<Result<(), String>> {
        match self.backend.health()? {
            Ok(()) => {
                if self.status == RuntimeStatus::Gone {
                    self.status = RuntimeStatus::Discovered;
                }
                Ok(Ok(()))
            }
            Err(reason) => {
                if self.status != RuntimeStatus::Gone {
                    self.status = RuntimeStatus::Unhealthy {
                        reason: reason.clone(),
                    };
                }
                Ok(Err(reason))
            }
        }
    }

    /// Readiness is a probe result, not a state assumption: Loaded runtimes
    /// must pass health() before Ready is returned, and an Unhealthy
    /// runtime is re-probed — a transient failure is not a permanent gate.
    pub fn ready(&mut self) -> EaiResult<bool> {
        let model = match &self.status {
            RuntimeStatus::Loaded { model } | RuntimeStatus::Ready { model } => model.clone(),
            RuntimeStatus::Unhealthy { .. } => match &self.loaded_model {
                Some(m) => m.clone(),
                None => return Ok(false),
            },
            RuntimeStatus::Discovered | RuntimeStatus::Gone => return Ok(false),
        };
        match self.backend.health()? {
            Ok(()) => {
                self.status = RuntimeStatus::Ready { model };
                Ok(true)
            }
            Err(reason) => {
                self.status = RuntimeStatus::Unhealthy { reason };
                Ok(false)
            }
        }
    }

    pub fn infer(&mut self, prompt: &str) -> EaiResult<InferOutcome> {
        let RuntimeStatus::Ready { .. } = self.status else {
            bail!(
                "runtime {} is not ready (status {:?}); process presence does not establish readiness",
                self.id,
                self.status
            );
        };
        let out = self.backend.infer(prompt)?;
        if let InferOutcome::Done { output } = &out {
            self.running.insert(prompt.to_string(), output.clone());
        }
        Ok(out)
    }

    pub fn cancel(&mut self) -> EaiResult<()> {
        if !self.caps.can_cancel {
            return Err(EaiError::config(format!(
                "runtime {} does not support {:?}",
                self.id,
                Op::Cancel
            )));
        }
        self.backend.cancel()
    }

    pub fn unload(&mut self) -> EaiResult<()> {
        if !self.caps.can_unload {
            return Err(EaiError::config(format!(
                "runtime {} does not support {:?}",
                self.id,
                Op::Unload
            )));
        }
        self.backend.unload()?;
        if self.status != RuntimeStatus::Gone {
            self.status = RuntimeStatus::Discovered;
        }
        Ok(())
    }
}
