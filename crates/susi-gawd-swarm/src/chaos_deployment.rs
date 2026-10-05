//! Deterministic chaos drills for isolated development deployments.
//!
//! Each instance owns its state, credential set, storage budget, and trace.
//! Fault injection cannot touch a neighboring deployment or the host running
//! the test. The model exercises the recovery contract a dev deployment must
//! expose: explicit faults, bounded recovery, and no credential leakage.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Faults injected by the isolated development-deployment drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevFault {
    /// The deployment has no space for the next state write.
    DiskFull,
    /// Volatile runtime state disappears and must be rebuilt.
    RuntimeCrash,
    /// Durable state is unreadable and must be restored from a checkpoint.
    CorruptState,
    /// The credential that started the deployment can no longer authorize it.
    RevokedCredential,
}

/// Explicit state reported while a deployment is recovering from a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DegradedReason {
    /// A state write hit the deployment's storage ceiling.
    DiskFull,
    /// The runtime stopped unexpectedly.
    RuntimeCrash,
    /// Durable state failed validation.
    CorruptState,
    /// The active credential was revoked.
    RevokedCredential,
}

/// Lifecycle state of an isolated development deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentState {
    /// The deployment has not been started.
    Stopped,
    /// The deployment is serving normally.
    Ready,
    /// The deployment is serving no unsafe work until the named fault is fixed.
    Degraded(DegradedReason),
    /// The runtime is rebuilding volatile state.
    Recovering,
}

/// A typed failure from a deployment operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentError {
    /// The deployment is not ready to perform the requested operation.
    NotReady,
    /// The operation would exceed the deployment's storage ceiling.
    DiskFull,
    /// The operation would exceed the deployment's mission budget.
    BudgetExhausted,
    /// The supplied credential is revoked or does not match the deployment.
    CredentialRejected,
    /// The state cannot be used until recovery is complete.
    CorruptState,
}

/// A bounded resource ledger for one isolated deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    limit: u64,
    spent: u64,
}

impl Budget {
    fn new(limit: u64) -> Self {
        Self { limit, spent: 0 }
    }

    fn try_consume(&mut self, cost: u64) -> Result<(), DeploymentError> {
        let next = self
            .spent
            .checked_add(cost)
            .ok_or(DeploymentError::BudgetExhausted)?;
        if next > self.limit {
            return Err(DeploymentError::BudgetExhausted);
        }
        self.spent = next;
        Ok(())
    }

    /// The maximum amount this deployment may spend.
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// The amount successfully spent so far.
    #[must_use]
    pub fn spent(&self) -> u64 {
        self.spent
    }
}

/// One isolated development deployment under chaos testing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevDeployment {
    namespace: String,
    state: DeploymentState,
    budget: Budget,
    storage_limit: u64,
    storage_used: u64,
    files: BTreeMap<String, Vec<u8>>,
    checkpoint: BTreeMap<String, Vec<u8>>,
    credential: String,
    revoked_credentials: BTreeSet<String>,
    active_credential: Option<String>,
    trace: Vec<String>,
    recovery_steps: u32,
    corrupt: bool,
}

impl DevDeployment {
    /// Create a stopped deployment with independent storage, identity, and budget.
    #[must_use]
    pub fn new(
        namespace: impl Into<String>,
        credential: impl Into<String>,
        budget_limit: u64,
        storage_limit: u64,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            state: DeploymentState::Stopped,
            budget: Budget::new(budget_limit),
            storage_limit,
            storage_used: 0,
            files: BTreeMap::new(),
            checkpoint: BTreeMap::new(),
            credential: credential.into(),
            revoked_credentials: BTreeSet::new(),
            active_credential: None,
            trace: Vec::new(),
            recovery_steps: 0,
            corrupt: false,
        }
    }

    /// Start the deployment and charge only the successful startup operation.
    pub fn start(&mut self, credential: &str, cost: u64) -> Result<(), DeploymentError> {
        if credential != self.credential
            || self.revoked_credentials.contains(credential)
            || self.state != DeploymentState::Stopped
        {
            return Err(DeploymentError::CredentialRejected);
        }
        self.budget.try_consume(cost)?;
        self.active_credential = Some(credential.to_owned());
        self.state = DeploymentState::Ready;
        self.trace.push(format!("{} started", self.namespace));
        Ok(())
    }

    /// Write a non-secret artifact, preserving prior state on failure.
    pub fn write_artifact(
        &mut self,
        name: impl Into<String>,
        contents: &[u8],
        cost: u64,
    ) -> Result<(), DeploymentError> {
        if self.state != DeploymentState::Ready {
            return Err(match self.state {
                DeploymentState::Degraded(DegradedReason::CorruptState) => {
                    DeploymentError::CorruptState
                }
                DeploymentState::Stopped
                | DeploymentState::Ready
                | DeploymentState::Degraded(DegradedReason::DiskFull)
                | DeploymentState::Degraded(DegradedReason::RuntimeCrash)
                | DeploymentState::Degraded(DegradedReason::RevokedCredential)
                | DeploymentState::Recovering => DeploymentError::NotReady,
            });
        }
        let size = u64::try_from(contents.len()).unwrap_or(u64::MAX);
        let next_size = self
            .storage_used
            .checked_add(size)
            .ok_or(DeploymentError::DiskFull)?;
        if next_size > self.storage_limit {
            self.degrade(DegradedReason::DiskFull);
            return Err(DeploymentError::DiskFull);
        }
        self.budget.try_consume(cost)?;
        let name = name.into();
        self.files.insert(name.clone(), contents.to_vec());
        self.storage_used = next_size;
        self.checkpoint = self.files.clone();
        self.trace.push(format!("{} committed", name));
        Ok(())
    }

    /// Inject a fault and expose its corresponding degraded state.
    pub fn inject(&mut self, fault: DevFault) {
        match fault {
            DevFault::DiskFull => self.degrade(DegradedReason::DiskFull),
            DevFault::RuntimeCrash => {
                self.state = DeploymentState::Recovering;
                self.trace.push("runtime crash injected".to_string());
            }
            DevFault::CorruptState => {
                self.corrupt = true;
                self.degrade(DegradedReason::CorruptState);
            }
            DevFault::RevokedCredential => {
                if let Some(credential) = self.active_credential.clone() {
                    self.revoked_credentials.insert(credential);
                }
                self.degrade(DegradedReason::RevokedCredential);
            }
        }
    }

    /// Recover a deployment using at most three bounded state transitions.
    pub fn recover(&mut self) -> Result<(), DeploymentError> {
        self.recovery_steps = self.recovery_steps.saturating_add(1);
        if self.recovery_steps > 3 {
            return Err(DeploymentError::NotReady);
        }
        match self.state {
            DeploymentState::Recovering => {
                self.state = DeploymentState::Ready;
                self.trace.push("runtime restarted".to_string());
                Ok(())
            }
            DeploymentState::Degraded(DegradedReason::RuntimeCrash) => {
                self.state = DeploymentState::Ready;
                self.trace.push("runtime restarted".to_string());
                Ok(())
            }
            DeploymentState::Degraded(DegradedReason::DiskFull) => {
                self.storage_limit = self.storage_limit.max(self.storage_used.saturating_add(1));
                self.state = DeploymentState::Ready;
                self.trace.push("storage recovered".to_string());
                Ok(())
            }
            DeploymentState::Degraded(DegradedReason::CorruptState) => {
                self.files = self.checkpoint.clone();
                self.storage_used = self
                    .files
                    .values()
                    .map(|contents| u64::try_from(contents.len()).unwrap_or(u64::MAX))
                    .sum();
                self.corrupt = false;
                self.state = DeploymentState::Ready;
                self.trace.push("checkpoint restored".to_string());
                Ok(())
            }
            DeploymentState::Degraded(DegradedReason::RevokedCredential) => {
                Err(DeploymentError::CredentialRejected)
            }
            DeploymentState::Ready => Ok(()),
            DeploymentState::Stopped => Err(DeploymentError::NotReady),
        }
    }

    /// Replace a revoked credential without exposing the old credential.
    pub fn reauthorize(&mut self, credential: impl Into<String>) -> Result<(), DeploymentError> {
        let credential = credential.into();
        if credential.is_empty() || self.revoked_credentials.contains(&credential) {
            return Err(DeploymentError::CredentialRejected);
        }
        self.credential = credential.clone();
        self.active_credential = Some(credential);
        self.state = DeploymentState::Ready;
        self.trace.push("credential reauthorized".to_string());
        Ok(())
    }

    fn degrade(&mut self, reason: DegradedReason) {
        self.state = DeploymentState::Degraded(reason);
        self.trace.push(format!("deployment degraded: {reason:?}"));
    }

    /// The deployment's namespace, used to prove isolation between instances.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The current explicit lifecycle state.
    #[must_use]
    pub fn state(&self) -> DeploymentState {
        self.state
    }

    /// The bounded resource ledger for this deployment.
    #[must_use]
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Number of recovery transitions attempted by this deployment.
    #[must_use]
    pub fn recovery_steps(&self) -> u32 {
        self.recovery_steps
    }

    /// Whether state validation is currently failing.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        self.corrupt
    }

    /// Redacted diagnostic trace. Credential values never enter this trace.
    #[must_use]
    pub fn trace(&self) -> &[String] {
        &self.trace
    }
}

/// The result of one isolated deployment chaos scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosScenario {
    /// The injected fault.
    pub fault: DevFault,
    /// State observed immediately after injection.
    pub degraded_state: DeploymentState,
    /// State observed after the recovery path.
    pub recovered_state: DeploymentState,
    /// Whether the fault recovered within the bounded step budget.
    pub bounded_recovery: bool,
    /// Whether the trace contains no credential material.
    pub privacy_preserved: bool,
    /// Whether the failed operation left the spend unchanged.
    pub budget_preserved: bool,
    /// Whether the peer deployment stayed independent and ready.
    pub isolation_preserved: bool,
    /// Ordered, redacted diagnostic steps.
    pub trace: Vec<String>,
}

/// The complete deterministic development-deployment chaos report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChaosReport {
    /// One result for each required deployment fault.
    pub scenarios: Vec<ChaosScenario>,
}

impl ChaosReport {
    /// Return true only when every required guard holds.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.scenarios.iter().all(|scenario| {
            scenario.bounded_recovery
                && scenario.privacy_preserved
                && scenario.budget_preserved
                && scenario.isolation_preserved
        })
    }
}

fn scenario(fault: DevFault, index: usize) -> ChaosScenario {
    let credential = format!("secret-{index}");
    let mut deployment = DevDeployment::new(format!("dev-chaos-{index}"), &credential, 100, 128);
    let mut peer = DevDeployment::new(format!("dev-peer-{index}"), "peer-secret", 100, 128);
    let _ = deployment.start(&credential, 10);
    let _ = peer.start("peer-secret", 10);
    let _ = deployment.write_artifact("checkpoint", b"private-state", 5);
    let _ = peer.write_artifact("peer-state", b"peer-state", 5);
    let before_fault_spend = deployment.budget.spent();
    deployment.inject(fault);
    let degraded_state = deployment.state;
    let fault_spend = deployment.budget.spent();

    let recovered = if fault == DevFault::RevokedCredential {
        deployment.recover().is_err()
            && deployment
                .reauthorize(format!("replacement-{index}"))
                .is_ok()
    } else {
        deployment.recover().is_ok()
    };
    let recovered_state = deployment.state;
    let privacy_preserved = deployment
        .trace
        .iter()
        .all(|entry| !entry.contains(&credential));
    let isolation_preserved = peer.state == DeploymentState::Ready
        && peer.budget.spent() == 15
        && peer.namespace != deployment.namespace;

    ChaosScenario {
        fault,
        degraded_state,
        recovered_state,
        bounded_recovery: recovered && deployment.recovery_steps <= 3,
        privacy_preserved,
        budget_preserved: before_fault_spend == fault_spend
            && deployment.budget.spent() >= before_fault_spend,
        isolation_preserved,
        trace: deployment.trace,
    }
}

/// Run repeatable chaos drills against independent, in-memory dev deployments.
#[must_use]
pub fn run_dev_matrix() -> ChaosReport {
    let faults = [
        DevFault::DiskFull,
        DevFault::RuntimeCrash,
        DevFault::CorruptState,
        DevFault::RevokedCredential,
    ];
    ChaosReport {
        scenarios: faults
            .into_iter()
            .enumerate()
            .map(|(index, fault)| scenario(fault, index))
            .collect(),
    }
}
