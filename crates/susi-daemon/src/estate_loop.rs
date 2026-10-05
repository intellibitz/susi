//! The unattended estate reconciliation loop (T-DEEPSEEK-160, VC-202-024).
//!
//! Each pass observes the local estate — resident model weights,
//! supervised leaf services, cooled provider credentials — diffs it
//! against the declared [`DesiredState`], and acts within policy: warm or
//! evict a model, start or drain a runtime, rotate away from a dead key,
//! collect an orphaned weight. Every pass and every action is appended to
//! a signed audit chain, and the pass journal makes a crash a resume,
//! not a restart of the work: actions already applied are never re-run.
//!
//! Actuation goes through the same seams the operator paths use —
//! `InferenceHost::preload`/`unload` for weights, `service_table`'s stop
//! flag plus SIGTERM for runtimes (the supervisor respawns a cleared stop
//! and holds a set one down), and the routing arbiter's vendor-scope
//! quarantine for dead credentials. An action policy cannot take is
//! recorded `Held`, never silently skipped.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::susi_config::desired_state::DesiredState;
use crate::susi_error::{EaiError, EaiResult};

/// Bounded history kept in the loop journal — the recent record an
/// operator consults without reading the whole audit chain.
const HISTORY_KEEP: usize = 32;
/// Estate pass cadence when nothing configures one.
const DEFAULT_INTERVAL_SECS: u64 = 300;

/// The kinds of reconciliation act the loop can take. Serialized names
/// are what `desired-state.json` policy lists (`act`/`hold`) and audit
/// records carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Stage a declared model's weights so the next request is warm.
    WarmModel,
    /// Evict a model the declared state marks cold.
    EvictModel,
    /// Clear a supervised leaf service's stop flag; the supervisor
    /// respawns it on its next probe.
    StartRuntime,
    /// Stop a supervised leaf service and hold it down — the inverse of
    /// start, for runtimes the declared state marks stopped.
    DrainRuntime,
    /// Quarantine a credential scope whose key the estate observed dead,
    /// so routing rotates away to a live credential.
    RotateKey,
    /// Evict a resident weight no desired model declares — the estate's
    /// own garbage collection. Foreign processes are never collected.
    CollectOrphan,
}

impl ActionKind {
    /// The serialized name — the same snake_case the JSON record carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WarmModel => "warm_model",
            Self::EvictModel => "evict_model",
            Self::StartRuntime => "start_runtime",
            Self::DrainRuntime => "drain_runtime",
            Self::RotateKey => "rotate_key",
            Self::CollectOrphan => "collect_orphan",
        }
    }

    /// How big the blast radius is when the act goes wrong — the
    /// autonomy tier the change defaults to. Low-blast acts are cheap
    /// and reversible (warm a cache, free memory, respawn a supervised
    /// service); high-blast acts interrupt a running service or move a
    /// credential — they need an explicit operator grant unless policy
    /// says otherwise.
    #[must_use]
    pub fn blast(self) -> Blast {
        match self {
            Self::WarmModel | Self::EvictModel | Self::StartRuntime | Self::CollectOrphan => {
                Blast::Low
            }
            Self::DrainRuntime | Self::RotateKey => Blast::High,
        }
    }
}

/// The blast-radius class an autonomy tier keys on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blast {
    /// Cheap, reversible, invisible to the operator when it works.
    Low,
    /// Interrupts a running service or moves a credential.
    High,
}

/// One planned reconciliation act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub kind: ActionKind,
    /// The resource the act names: a model id, a leaf service name, or a
    /// provider id.
    pub target: String,
    /// Why the diff produced this act — carried into receipts and audit.
    pub reason: String,
}

impl Action {
    /// The idempotency key the pass journal dedupes on.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.target)
    }
}

/// What one act's execution concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The act ran through its production seam.
    Applied(String),
    /// The act ran through its production seam under a one-shot
    /// operator grant the loop consumed first.
    Approved(String),
    /// The act was rehearsed only — the receipt and audit name what
    /// would have run, and nothing was actuated.
    DryRun(String),
    /// The act waits on an operator grant; the detail names the grant
    /// file an approval writes.
    AwaitingApproval(String),
    /// Policy or ownership held it — recorded, never silently skipped.
    Held(String),
    /// The seam refused — the reason is on the receipt and the audit.
    Failed(String),
}

/// A finished act: what was planned, what the seam or policy decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub action: Action,
    pub verdict: Verdict,
    pub unix: u64,
}

/// One supervised leaf service's observed state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceObs {
    pub name: String,
    /// The service's port answers right now.
    pub up: bool,
    /// The operator/loop stop flag is set — the supervisor holds it down.
    pub stopped: bool,
    /// The port is held by a process the daemon did not spawn. Never
    /// signalled, never collected.
    pub external: bool,
}

/// The estate as one pass sees it — injected so tests drive the same
/// planning and execution code the production tick runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// Model ids with weights resident in the inference substrate.
    pub loaded_models: Vec<String>,
    /// Every supervised leaf service's table row.
    pub services: Vec<ServiceObs>,
    /// Provider/vendor names inside a credential cooldown right now.
    pub cooled_providers: Vec<String>,
}

/// Read the real estate: the substrate cache's resident weights, the
/// leaf-service process table, and the routing arbiter's cooldowns.
/// `config_dir` is only used by probes that need it; everything read here
/// is daemon-local.
#[must_use]
pub fn observe() -> Observed {
    let loaded_models = susi_gemi::engines::runtime::InferenceHost::loaded_models()
        .iter()
        .filter_map(|entry| {
            entry
                .get("path")
                .and_then(|p| p.as_str())
                .and_then(|p| Path::new(p).file_stem())
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    let services = susi_core::service_table::status()
        .iter()
        .map(|s| ServiceObs {
            name: s.name.to_string(),
            up: s.up,
            stopped: s.stopped,
            external: s.external,
        })
        .collect();
    let cooled_providers = susi_gemi::engines::routing::InferenceRouter::cooled_providers()
        .iter()
        .map(|c| c.provider.clone())
        .collect();
    Observed {
        loaded_models,
        services,
        cooled_providers,
    }
}

/// The autonomy tier one act resolves to — who may run it without an
/// operator in the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// Actuate through the production seam.
    Act,
    /// Rehearse and record the act; never actuate.
    DryRun,
    /// Actuate only when a durable operator grant names this action.
    Ask,
}

/// `policy.estate` from the desired document resolves each act to a
/// tier. Explicit per-kind lists outrank global autonomy:
/// `"hold": [kinds]` denies, `"dry_run": [kinds]` rehearses,
/// `"ask": [kinds]` waits on a grant, `"act": [kinds]` is an allowlist
/// (an unlisted kind is held), `"autonomy": "report"` holds everything
/// and `"autonomy": "act"` promotes every unlisted kind to act. Absent
/// all of these the blast radius decides: low-blast acts run, high-blast
/// ones ask — declaring the state is consent to keep it, not to take a
/// service down or move a credential.
fn tier_of(desired: &DesiredState, kind: ActionKind) -> Result<Tier, String> {
    let estate = desired.policy.get("estate");
    let listed = |key: &str| {
        estate
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_array())
            .is_some_and(|list| {
                list.iter()
                    .filter_map(|v| v.as_str())
                    .any(|k| k == kind.as_str())
            })
    };
    if listed("hold") {
        return Err(format!("estate.hold lists {}", kind.as_str()));
    }
    let autonomy = estate
        .and_then(|p| p.get("autonomy"))
        .and_then(|v| v.as_str());
    if autonomy == Some("report") {
        return Err("estate.autonomy is report".to_string());
    }
    if listed("dry_run") {
        return Ok(Tier::DryRun);
    }
    if listed("ask") {
        return Ok(Tier::Ask);
    }
    if autonomy == Some("act") {
        return Ok(Tier::Act);
    }
    if let Some(list) = estate.and_then(|p| p.get("act")).and_then(|v| v.as_array()) {
        let kinds: BTreeSet<&str> = list.iter().filter_map(|v| v.as_str()).collect();
        return if kinds.contains(kind.as_str()) {
            Ok(Tier::Act)
        } else {
            Err(format!("estate.act omits {}", kind.as_str()))
        };
    }
    Ok(match kind.blast() {
        Blast::Low => Tier::Act,
        Blast::High => Tier::Ask,
    })
}

/// Diff the declared state against the observed estate into the acts a
/// pass should take. Undeclared resources are left alone except resident
/// weights, which are the estate's own cache and safe to collect;
/// external processes are never touched.
#[must_use]
pub fn plan(desired: &DesiredState, observed: &Observed) -> Vec<Action> {
    let mut out = Vec::new();
    let loaded: BTreeSet<&str> = observed.loaded_models.iter().map(String::as_str).collect();
    let declared_models: BTreeSet<&str> = desired.models.iter().map(|m| m.id.as_str()).collect();

    for m in &desired.models {
        let kind = if m.kind.is_empty() {
            "warm"
        } else {
            m.kind.as_str()
        };
        match kind {
            "warm" | "loaded" if !loaded.contains(m.id.as_str()) => out.push(Action {
                kind: ActionKind::WarmModel,
                target: m.id.clone(),
                reason: "declared warm, weights not resident".to_string(),
            }),
            "cold" | "evicted" | "stopped" if loaded.contains(m.id.as_str()) => {
                out.push(Action {
                    kind: ActionKind::EvictModel,
                    target: m.id.clone(),
                    reason: "declared cold, weights resident".to_string(),
                });
            }
            _ => {}
        }
    }
    // Resident weights no desired model declares are orphans — the
    // estate's own cache, so collecting them is always safe. Only a
    // present desired document creates this duty; absent one, the loop
    // never plans at all.
    for id in &observed.loaded_models {
        if !declared_models.contains(id.as_str()) {
            out.push(Action {
                kind: ActionKind::CollectOrphan,
                target: id.clone(),
                reason: "resident weights no desired model declares".to_string(),
            });
        }
    }

    for r in &desired.runtimes {
        let Some(svc) = observed.services.iter().find(|s| s.name == r.id) else {
            out.push(Action {
                kind: ActionKind::StartRuntime,
                target: r.id.clone(),
                reason: "declared runtime is not a supervised leaf service".to_string(),
            });
            continue;
        };
        let kind = if r.kind.is_empty() {
            "running"
        } else {
            r.kind.as_str()
        };
        match kind {
            "stopped" | "drained" if svc.up => out.push(Action {
                kind: ActionKind::DrainRuntime,
                target: r.id.clone(),
                reason: "declared stopped, runtime is up".to_string(),
            }),
            _ if !svc.up || svc.stopped => out.push(Action {
                kind: ActionKind::StartRuntime,
                target: r.id.clone(),
                reason: "declared running, runtime is down".to_string(),
            }),
            _ => {}
        }
    }

    for p in &desired.providers {
        let dead = observed
            .cooled_providers
            .iter()
            .any(|c| c == &p.id || format!("vendor:{c}") == format!("vendor:{}", p.id));
        if dead {
            out.push(Action {
                kind: ActionKind::RotateKey,
                target: p.id.clone(),
                reason: "declared live, credential scope is cooled".to_string(),
            });
        }
    }
    out
}

/// The mid-pass resume cursor: which pass is in flight, what it planned,
/// and which action ids already applied — a crash resumes here rather
/// than restarting the work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InFlight {
    pub pass: u64,
    pub started_unix: u64,
    pub plan: Vec<Action>,
    #[serde(default)]
    pub applied: Vec<String>,
}

/// One finished pass's compact record, kept bounded in the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassSummary {
    pub pass: u64,
    pub started_unix: u64,
    pub finished_unix: u64,
    pub planned: usize,
    pub applied: usize,
    pub held: usize,
    pub failed: usize,
    #[serde(default)]
    pub dry_run: usize,
    #[serde(default)]
    pub awaiting: usize,
}

/// The durable loop journal: `<home>/estate-loop.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopState {
    pub last_completed: u64,
    #[serde(default)]
    pub in_flight: Option<InFlight>,
    #[serde(default)]
    pub history: Vec<PassSummary>,
}

impl LoopState {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// # Errors
    /// [`EaiError::io`] when the journal cannot be written.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
        }
        let body = serde_json::to_vec(self).map_err(|e| EaiError::io(e.to_string()))?;
        crate::susi_config::atomic_write_bytes(path, &body)
            .map_err(|e| EaiError::filesystem(format!("estate loop journal: {e}")))
    }
}

/// The full report a pass returns — also what the audit chain records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassReport {
    pub pass: u64,
    /// True when this pass resumed a crash-interrupted one.
    pub resumed: bool,
    pub started_unix: u64,
    pub finished_unix: u64,
    pub receipts: Vec<ActionReceipt>,
}

impl PassReport {
    #[must_use]
    pub fn summary(&self) -> PassSummary {
        PassSummary {
            pass: self.pass,
            started_unix: self.started_unix,
            finished_unix: self.finished_unix,
            planned: self.receipts.len(),
            applied: self
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::Applied(_) | Verdict::Approved(_)))
                .count(),
            held: self
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::Held(_)))
                .count(),
            failed: self
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::Failed(_)))
                .count(),
            dry_run: self
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::DryRun(_)))
                .count(),
            awaiting: self
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::AwaitingApproval(_)))
                .count(),
        }
    }
}

/// Where the loop's files live under the substrate home.
#[must_use]
pub fn state_path(home: &Path) -> PathBuf {
    home.join("estate-loop.json")
}
#[must_use]
pub fn desired_path(home: &Path) -> PathBuf {
    home.join("desired-state.json")
}
#[must_use]
pub fn audit_path(home: &Path) -> PathBuf {
    home.join("estate-audit.log")
}

/// Where one-shot operator grants live — `<home>/estate-approvals/`.
/// A grant is a file named for the action id it approves; the loop
/// consumes it before actuating so a crash never re-runs a costly
/// change on a stale approval.
#[must_use]
pub fn approvals_dir(home: &Path) -> PathBuf {
    home.join("estate-approvals")
}

fn grant_path(home: &Path, action_id: &str) -> PathBuf {
    let name: String = action_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    approvals_dir(home).join(format!("{name}.json"))
}

/// Grant an `ask`-tier action a one-shot approval: write the durable
/// grant the next pass consumes before it actuates. The grant names
/// the action and the approval time — it is evidence, not just a flag.
///
/// # Errors
/// [`EaiError::io`] when the grant file cannot be written.
pub fn approve(home: &Path, action_id: &str, now: u64) -> EaiResult<PathBuf> {
    let dir = approvals_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| EaiError::io(e.to_string()))?;
    let path = grant_path(home, action_id);
    let body = serde_json::to_vec(&serde_json::json!({
        "action": action_id,
        "granted_unix": now,
    }))
    .map_err(|e| EaiError::io(e.to_string()))?;
    crate::susi_config::atomic_write_bytes(&path, &body)
        .map_err(|e| EaiError::filesystem(format!("estate approval grant: {e}")))?;
    Ok(path)
}

/// Consume a grant if one names this action — one-shot, so the file is
/// removed whether or not the act then succeeds. Returns true when a
/// grant existed.
fn consume_grant(home: &Path, action_id: &str) -> bool {
    let path = grant_path(home, action_id);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        true
    } else {
        false
    }
}

/// Estate pass cadence — `SUSI_ESTATE_INTERVAL_SECS`, default 5 minutes.
#[must_use]
pub fn interval_secs() -> u64 {
    std::env::var("SUSI_ESTATE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// The actuation seam: production uses [`SystemActuate`]; tests inject a
/// recorder so the same plan/policy/journal/audit code runs hermetically.
pub trait Actuate: Send + Sync {
    fn act(&self, action: &Action) -> Verdict;
}

/// Send a unix signal to a pid the daemon supervises.
// SAFETY: libc::kill sends a signal and touches no memory; the pid comes
// from our own process table (a child the daemon spawned).
#[cfg(unix)]
#[allow(unsafe_code)]
fn signal(pid: u32, sig: i32) -> bool {
    unsafe { libc::kill(pid as i32, sig) == 0 }
}

#[cfg(not(unix))]
fn signal(pid: u32, _sig: i32) -> bool {
    let _ = (pid, _sig);
    false
}

#[cfg(unix)]
const SIGTERM: i32 = libc::SIGTERM;
#[cfg(not(unix))]
const SIGTERM: i32 = 15;

/// Production actuation — the same seams the operator surfaces use.
pub struct SystemActuate;

impl Actuate for SystemActuate {
    fn act(&self, action: &Action) -> Verdict {
        match action.kind {
            ActionKind::WarmModel => {
                match susi_gemi::engines::runtime::InferenceHost::preload(&action.target) {
                    Ok(v) => Verdict::Applied(format!("preloaded: {v}")),
                    Err(e) => Verdict::Failed(format!("preload refused: {e}")),
                }
            }
            ActionKind::EvictModel | ActionKind::CollectOrphan => {
                match susi_gemi::engines::runtime::InferenceHost::unload(&action.target) {
                    Ok(v) => Verdict::Applied(format!("unloaded: {v}")),
                    Err(e) => Verdict::Failed(format!("unload refused: {e}")),
                }
            }
            ActionKind::StartRuntime => {
                if susi_core::service_table::leaf_service(&action.target).is_none() {
                    return Verdict::Held(format!(
                        "{} is not a leaf service — the loop starts only supervised runtimes",
                        action.target
                    ));
                }
                let cleared = susi_core::service_table::update_with(|table| {
                    table
                        .iter_mut()
                        .find(|r| r.name == action.target)
                        .map(|rec| {
                            if rec.stopped {
                                rec.stopped = false;
                                rec.disabled_until = None;
                                true
                            } else {
                                false
                            }
                        })
                });
                match cleared {
                    Ok(Some(true)) => Verdict::Applied(
                        "stop flag cleared; supervisor respawns on its next probe".to_string(),
                    ),
                    Ok(Some(false)) | Ok(None) => {
                        Verdict::Applied("already up or unsupervised".to_string())
                    }
                    Err(e) => Verdict::Failed(format!("process table refused: {e}")),
                }
            }
            ActionKind::DrainRuntime => {
                if susi_core::service_table::leaf_service(&action.target).is_none() {
                    return Verdict::Held(format!(
                        "{} is not a leaf service — the loop drains only supervised runtimes",
                        action.target
                    ));
                }
                let row = susi_core::service_table::update_with(|table| {
                    table
                        .iter_mut()
                        .find(|r| r.name == action.target)
                        .map(|rec| {
                            rec.stopped = true;
                            (rec.pid, rec.external)
                        })
                });
                match row {
                    Ok(Some((_pid, true))) => Verdict::Held(
                        "bound by an external process — never signal what we did not spawn"
                            .to_string(),
                    ),
                    Ok(Some((pid, false))) => {
                        let signalled = pid == 0
                            || !susi_core::service_table::pid_alive(pid)
                            || signal(pid, SIGTERM);
                        Verdict::Applied(if signalled {
                            format!("stop flag set; SIGTERM pid {pid}")
                        } else {
                            format!("stop flag set; SIGTERM pid {pid} failed")
                        })
                    }
                    Ok(None) => Verdict::Applied("no process-table row — already down".to_string()),
                    Err(e) => Verdict::Failed(format!("process table refused: {e}")),
                }
            }
            ActionKind::RotateKey => {
                susi_gemi::engines::routing::InferenceRouter::record_vendor_failure(&action.target);
                Verdict::Applied(
                    "credential scope quarantined; routing rotates to the next credential"
                        .to_string(),
                )
            }
        }
    }
}

fn audit(home: &Path, component: &str, details: &str) {
    let _ = crate::susi_sandbox::audit_chain::append_signed_entry(
        &audit_path(home),
        "INFO",
        component,
        details,
        std::process::id(),
    );
}

/// One reconciliation pass over an observed estate — the seam the
/// production tick and the tests share. `desired` is `None` when no
/// desired-state document exists: the loop plans nothing (an absent
/// declaration is not consent to manage anything), and the empty pass
/// is still audited.
///
/// A crash mid-pass leaves `in_flight` in the journal; the next pass
/// resumes that plan at its first unapplied action instead of planning
/// fresh work.
///
/// # Errors
/// [`EaiError::io`] when the loop journal cannot be persisted.
pub fn run_pass_with(
    home: &Path,
    desired: Option<&DesiredState>,
    observed: &Observed,
    now: u64,
    actuate: &dyn Actuate,
) -> EaiResult<PassReport> {
    let path = state_path(home);
    let mut state = LoopState::load(&path);
    let resumed = state.in_flight.is_some();
    let mut flight = state.in_flight.take().unwrap_or_else(|| InFlight {
        pass: state.last_completed.saturating_add(1),
        started_unix: now,
        plan: desired.map(|d| plan(d, observed)).unwrap_or_default(),
        applied: Vec::new(),
    });
    let pass = flight.pass;
    let started_unix = flight.started_unix;

    let mut receipts = Vec::new();
    let pending: Vec<Action> = flight
        .plan
        .iter()
        .filter(|a| !flight.applied.contains(&a.id()))
        .cloned()
        .collect();
    for action in pending {
        let verdict = match desired {
            None => Verdict::Held("no desired-state document".to_string()),
            Some(d) => match tier_of(d, action.kind) {
                Err(why) => Verdict::Held(why),
                Ok(Tier::DryRun) => Verdict::DryRun(format!("{}: {}", action.id(), action.reason)),
                Ok(Tier::Ask) => {
                    if consume_grant(home, &action.id()) {
                        audit(
                            home,
                            "ESTATE_APPROVAL",
                            &format!("pass={pass} action={} grant consumed", action.id()),
                        );
                        match actuate.act(&action) {
                            Verdict::Applied(d) => Verdict::Approved(d),
                            v @ Verdict::Failed(_) => {
                                // The seam refused — the side effect
                                // never landed, so re-arm the grant.
                                let _ = approve(home, &action.id(), now);
                                v
                            }
                            v @ (Verdict::Approved(_)
                            | Verdict::DryRun(_)
                            | Verdict::AwaitingApproval(_)
                            | Verdict::Held(_)) => v,
                        }
                    } else {
                        Verdict::AwaitingApproval(format!(
                            "grant {} to authorize",
                            grant_path(home, &action.id()).display()
                        ))
                    }
                }
                Ok(Tier::Act) => actuate.act(&action),
            },
        };
        audit(
            home,
            "ESTATE_ACTION",
            &format!(
                "pass={pass} action={} verdict={}",
                action.id(),
                match &verdict {
                    Verdict::Applied(d) => format!("applied {d}"),
                    Verdict::Approved(d) => format!("approved {d}"),
                    Verdict::DryRun(d) => format!("dry-run {d}"),
                    Verdict::AwaitingApproval(d) => format!("awaiting {d}"),
                    Verdict::Held(d) => format!("held {d}"),
                    Verdict::Failed(d) => format!("failed {d}"),
                }
            ),
        );
        receipts.push(ActionReceipt {
            action: action.clone(),
            verdict,
            unix: now,
        });
        flight.applied.push(action.id());
        // Persist after every act — the applied list is the resume point.
        state.in_flight = Some(flight.clone());
        state.save(&path)?;
    }

    let report = PassReport {
        pass,
        resumed,
        started_unix,
        finished_unix: now,
        receipts,
    };
    audit(
        home,
        "ESTATE_PASS",
        &format!(
            "pass={} resumed={} planned={} applied={} held={} failed={} dry_run={} awaiting={}",
            report.pass,
            report.resumed,
            report.receipts.len(),
            report
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::Applied(_) | Verdict::Approved(_)))
                .count(),
            report
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::Held(_)))
                .count(),
            report
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::Failed(_)))
                .count(),
            report
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::DryRun(_)))
                .count(),
            report
                .receipts
                .iter()
                .filter(|r| matches!(r.verdict, Verdict::AwaitingApproval(_)))
                .count(),
        ),
    );
    state.last_completed = pass;
    state.in_flight = None;
    state.history.push(report.summary());
    if state.history.len() > HISTORY_KEEP {
        state.history.drain(..state.history.len() - HISTORY_KEEP);
    }
    state.save(&path)?;
    Ok(report)
}

/// The production pass: observe the real estate, load the declared state
/// if the operator wrote one, act through [`SystemActuate`].
///
/// # Errors
/// [`EaiError::io`] on journal or desired-state read/write failures; a
/// malformed desired document is a typed refusal, never a parse panic.
pub fn run_pass(home: &Path, now: u64) -> EaiResult<PassReport> {
    let desired = match std::fs::read_to_string(desired_path(home)) {
        Ok(text) => Some(crate::susi_config::desired_state::parse_desired_state(
            &text,
        )?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(EaiError::io(format!("read desired state: {e}"))),
    };
    let observed = observe();
    run_pass_with(home, desired.as_ref(), &observed, now, &SystemActuate)
}
