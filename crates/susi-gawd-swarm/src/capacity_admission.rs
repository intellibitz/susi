//! Measured-capacity admission for DAG scheduling (T-DEVIN-12).
//!
//! Production dispatch previously admitted nodes against fixed assumptions
//! (8 CPUs, 16 GiB GPU, `model_ready = true`). Admission now measures the
//! host each scheduling round: CPU parallelism, available system memory,
//! and GPU memory where a probe exists. Dimensions that cannot be measured
//! fail in the honest direction — unmeasured system memory does not gate
//! (`f64::MAX` sentinel), while unmeasured GPU memory is `0`, so a node
//! that actually needs a GPU queues instead of overcommitting.
//!
//! Operator overrides (`SUSI_ADMIT_*` env) are explicit escapes for
//! containers and CI, never defaults.

use crate::resource_schedule::{Admit, DagNode, Resources};

/// Minimum system memory for the bundled model runtime to be considered
/// ready — below this the host can still run tool-only nodes.
const MIN_MODEL_MEM_GB: f64 = 2.0;

/// A capacity snapshot provider. Production uses [`HostProbe`]; tests
/// inject fixed fixtures so scheduling decisions are deterministic.
pub trait CapacityProbe: Send + Sync {
    /// Live capacity this scheduling round. Called once per ready-batch so
    /// a draining mission re-admits deferred nodes under fresh headroom.
    fn measure(&self) -> Resources;
}

/// Probe backed by the host this process runs on.
#[derive(Debug, Default)]
pub struct HostProbe;

impl CapacityProbe for HostProbe {
    fn measure(&self) -> Resources {
        measured()
    }
}

/// A fixed snapshot for tests and for callers that already probed.
#[derive(Debug)]
pub struct FixedProbe(pub Resources);

impl CapacityProbe for FixedProbe {
    fn measure(&self) -> Resources {
        self.0.clone()
    }
}

/// Environment override escape hatch: `SUSI_ADMIT_CPU`, `SUSI_ADMIT_MEM_GB`,
/// `SUSI_ADMIT_GPU_GB`, `SUSI_ADMIT_MODEL_READY` (`0`/`1`/`true`/`false`).
fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn env_bool(key: &str) -> Option<bool> {
    match std::env::var(key).ok()?.as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// Host CPU parallelism — the honest worker ceiling for this dispatch.
fn host_cpu() -> Option<f64> {
    std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as f64)
}

/// `MemAvailable` from `/proc/meminfo`, in GiB. `None` on platforms without
/// a readable figure — callers treat `None` as unmeasurable, not zero.
fn host_mem_gb() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kib = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())?;
            return Some(kib / 1_048_576.0);
        }
    }
    None
}

/// GPU memory in GiB. No std-only device probe exists — the vendored candle
/// runtime is what would know; until that reports upward, operators state
/// capacity via `SUSI_ADMIT_GPU_GB`. Unmeasured returns `0` so GPU-hungry
/// nodes queue rather than overcommit.
fn host_gpu_gb() -> f64 {
    env_f64("SUSI_ADMIT_GPU_GB").unwrap_or(0.0)
}

/// Measure this host's schedulable capacity right now.
#[must_use]
pub fn measured() -> Resources {
    let cpu = env_f64("SUSI_ADMIT_CPU").or_else(host_cpu).unwrap_or(1.0);
    let mem_gb = env_f64("SUSI_ADMIT_MEM_GB")
        .or_else(host_mem_gb)
        .unwrap_or(f64::MAX);
    let gpu_mem_gb = host_gpu_gb();
    let model_ready =
        env_bool("SUSI_ADMIT_MODEL_READY").unwrap_or(cpu >= 1.0 && mem_gb >= MIN_MODEL_MEM_GB);
    let subprocesses = host_cpu()
        .and_then(|n| u32::try_from(n as u64).ok())
        .unwrap_or(1)
        .max(1);
    Resources {
        cpu,
        gpu_mem_gb,
        mem_gb,
        subprocesses,
        model_ready,
        tool_grants: vec!["exec_command".to_string()],
    }
}

/// One scheduling round: admit ready nodes against a measured snapshot,
/// reserving as admits succeed so concurrent admits never oversubscribe.
/// Deferred nodes are returned for the next round — they drain as running
/// workers finish, and a node whose request can never fit this host is
/// surfaced by the caller as a capacity error rather than spinning.
#[must_use]
pub fn admit_round(
    probe: &dyn CapacityProbe,
    ready: &[usize],
    requests: &std::collections::BTreeMap<usize, DagNode>,
    default_request: impl Fn(usize) -> DagNode,
) -> (Vec<usize>, Vec<usize>, Resources) {
    let mut remaining = probe.measure();
    let mut admitted = Vec::new();
    let mut deferred = Vec::new();
    for &idx in ready {
        let req = requests
            .get(&idx)
            .cloned()
            .unwrap_or_else(|| default_request(idx));
        match crate::resource_schedule::admit(&req, &remaining) {
            crate::resource_schedule::Admit::Run => {
                remaining = crate::resource_schedule::reserve(&remaining, &req);
                admitted.push(idx);
            }
            crate::resource_schedule::Admit::Queue => deferred.push(idx),
        }
    }
    (admitted, deferred, remaining)
}

/// A resource reservation that is released exactly once when its owner drops
/// it.  The token is deliberately the only way to decrement shared usage, so
/// failures and cancellation cannot leave a stale reservation behind.
pub struct ResourceReservation {
    admission: std::sync::Arc<LiveAdmissionInner>,
    id: u64,
}

impl std::fmt::Debug for ResourceReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceReservation")
            .field("id", &self.id)
            .finish()
    }
}

impl ResourceReservation {
    /// Stable identifier for logs and tests.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for ResourceReservation {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(footprint) = state.active.remove(&self.id) else {
            return;
        };
        if let Some(key) = state
            .active_keys
            .iter()
            .find_map(|(key, id)| (*id == self.id).then(|| key.clone()))
        {
            state.active_keys.remove(&key);
        }
        state.used.subtract(footprint);
        self.admission.ready.notify_all();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionBlock {
    /// Node that could not be admitted from the current measured inventory.
    pub node_id: String,
    /// Stable operator-facing reason; no node is reported complete on this
    /// path and a later call may drain the retained queue entry.
    pub reason: &'static str,
}

/// Result of one live admission pass.  `deferred` is intentionally retained
/// by the caller as work to retry; it is not a successful completion record.
pub struct AdmissionBatch {
    pub admitted: Vec<(usize, ResourceReservation)>,
    pub deferred: Vec<usize>,
    pub blocked: Option<AdmissionBlock>,
}

impl AdmissionBatch {
    #[must_use]
    pub fn admitted_indices(&self) -> Vec<usize> {
        self.admitted.iter().map(|(idx, _)| *idx).collect()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Footprint {
    cpu: f64,
    gpu_mem_gb: f64,
    mem_gb: f64,
    subprocesses: u32,
}

impl Footprint {
    fn from_node(node: &DagNode) -> Self {
        Self {
            cpu: node.cpu,
            gpu_mem_gb: node.gpu_mem_gb,
            mem_gb: node.mem_gb,
            subprocesses: node.subprocesses,
        }
    }
}

impl std::ops::AddAssign for Footprint {
    fn add_assign(&mut self, rhs: Self) {
        self.cpu += rhs.cpu;
        self.gpu_mem_gb += rhs.gpu_mem_gb;
        self.mem_gb += rhs.mem_gb;
        self.subprocesses = self.subprocesses.saturating_add(rhs.subprocesses);
    }
}

impl Footprint {
    fn subtract(&mut self, rhs: Self) {
        self.cpu = (self.cpu - rhs.cpu).max(0.0);
        self.gpu_mem_gb = (self.gpu_mem_gb - rhs.gpu_mem_gb).max(0.0);
        self.mem_gb = (self.mem_gb - rhs.mem_gb).max(0.0);
        self.subprocesses = self.subprocesses.saturating_sub(rhs.subprocesses);
    }
}

struct Pending {
    key: String,
    node: DagNode,
}

#[derive(Default)]
struct AdmissionState {
    used: Footprint,
    next_id: u64,
    active: std::collections::BTreeMap<u64, Footprint>,
    active_keys: std::collections::BTreeMap<String, u64>,
    pending: std::collections::VecDeque<Pending>,
    granted: std::collections::BTreeMap<String, u64>,
}

struct LiveAdmissionInner {
    probe: std::sync::Arc<dyn CapacityProbe>,
    state: std::sync::Mutex<AdmissionState>,
    ready: std::sync::Condvar,
}

/// Shared measured-capacity admission for all missions in one process.
///
/// Every request joins one fair queue. A release drains the queue against a
/// fresh probe snapshot, admitting the oldest fitting requests while skipping
/// a request whose own GPU/model/tool requirement is currently unavailable.
/// This preserves FIFO fairness without letting one impossible specialist
/// block unrelated work, and the reservation map makes simultaneous mission
/// admission atomic.
#[derive(Clone)]
pub struct LiveAdmission {
    inner: std::sync::Arc<LiveAdmissionInner>,
}

impl LiveAdmission {
    /// Build a controller around an injected live inventory.
    #[must_use]
    pub fn new(probe: std::sync::Arc<dyn CapacityProbe>) -> Self {
        Self {
            inner: std::sync::Arc::new(LiveAdmissionInner {
                probe,
                state: std::sync::Mutex::new(AdmissionState::default()),
                ready: std::sync::Condvar::new(),
            }),
        }
    }

    /// Build a controller from a fixed inventory, primarily for hermetic
    /// callers that already measured a host.
    #[must_use]
    pub fn fixed(resources: Resources) -> Self {
        Self::new(std::sync::Arc::new(FixedProbe(resources)))
    }

    /// Build a controller backed by the production host probe.
    #[must_use]
    pub fn host() -> Self {
        Self::new(std::sync::Arc::new(HostProbe))
    }

    /// Atomically enqueue and admit a ready batch for one mission. If another
    /// mission currently holds the needed capacity, this waits for a release;
    /// model/GPU/tool unavailability returns immediately as a queued block so
    /// the caller can retry after inventory changes.
    pub fn admit_batch(&self, mission: &str, requests: &[(usize, DagNode)]) -> AdmissionBatch {
        if requests.is_empty() {
            return AdmissionBatch {
                admitted: Vec::new(),
                deferred: Vec::new(),
                blocked: None,
            };
        }
        let keys: std::collections::BTreeSet<String> = requests
            .iter()
            .map(|(index, _)| Self::key(mission, *index))
            .collect();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        for (index, node) in requests {
            let key = Self::key(mission, *index);
            let already_queued = state.pending.iter().any(|p| p.key == key);
            if !already_queued
                && !state.granted.contains_key(&key)
                && !state.active_keys.contains_key(&key)
            {
                state.pending.push_back(Pending {
                    key,
                    node: node.clone(),
                });
            }
        }

        loop {
            self.drain_locked(&mut state);
            let mut admitted = Vec::new();
            for key in &keys {
                if let Some(id) = state.granted.remove(key) {
                    if let Some(pending) = requests
                        .iter()
                        .find(|(i, _)| Self::key(mission, *i) == *key)
                    {
                        let node = &pending.1;
                        state.active.insert(id, Footprint::from_node(node));
                        admitted.push((
                            pending.0,
                            ResourceReservation {
                                admission: std::sync::Arc::clone(&self.inner),
                                id,
                            },
                        ));
                    }
                }
            }
            if !admitted.is_empty() {
                let admitted_set: std::collections::BTreeSet<usize> =
                    admitted.iter().map(|(index, _)| *index).collect();
                let deferred = requests
                    .iter()
                    .map(|(index, _)| *index)
                    .filter(|index| !admitted_set.contains(index))
                    .collect();
                return AdmissionBatch {
                    admitted,
                    deferred,
                    blocked: None,
                };
            }

            let measured = self.inner.probe.measure();
            let possible = requests.iter().any(|(_, node)| {
                matches!(crate::resource_schedule::admit(node, &measured), Admit::Run)
            });
            if !possible {
                let (_index, node) = requests[0].clone();
                return AdmissionBatch {
                    admitted: Vec::new(),
                    deferred: requests.iter().map(|(idx, _)| *idx).collect(),
                    blocked: Some(AdmissionBlock {
                        node_id: node.id.clone(),
                        reason: block_reason(&node, &measured),
                    }),
                };
            }
            if state.active.is_empty() && state.granted.is_empty() {
                let (_index, node) = requests[0].clone();
                return AdmissionBatch {
                    admitted: Vec::new(),
                    deferred: requests.iter().map(|(idx, _)| *idx).collect(),
                    blocked: Some(AdmissionBlock {
                        node_id: node.id.clone(),
                        reason: "resource queue made no progress",
                    }),
                };
            }
            state = self
                .inner
                .ready
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Number of currently held reservations.
    #[must_use]
    pub fn active(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .len()
    }

    /// Number of requests waiting for a future drain.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }

    fn key(mission: &str, index: usize) -> String {
        format!("{mission}\0{index}")
    }

    fn drain_locked(&self, state: &mut AdmissionState) {
        let measured = self.inner.probe.measure();
        let mut free = Resources {
            cpu: (measured.cpu - state.used.cpu).max(0.0),
            gpu_mem_gb: (measured.gpu_mem_gb - state.used.gpu_mem_gb).max(0.0),
            mem_gb: (measured.mem_gb - state.used.mem_gb).max(0.0),
            subprocesses: measured
                .subprocesses
                .saturating_sub(state.used.subprocesses),
            model_ready: measured.model_ready,
            tool_grants: measured.tool_grants.clone(),
        };
        let pending_count = state.pending.len();
        for _ in 0..pending_count {
            let Some(pending) = state.pending.pop_front() else {
                break;
            };
            if matches!(
                crate::resource_schedule::admit(&pending.node, &free),
                Admit::Run
            ) {
                free = crate::resource_schedule::reserve(&free, &pending.node);
                state.next_id = state.next_id.saturating_add(1);
                state.granted.insert(pending.key.clone(), state.next_id);
                state.active_keys.insert(pending.key, state.next_id);
                let footprint = Footprint::from_node(&pending.node);
                state.used += footprint;
                // The request is marked active only when its caller takes the
                // grant, preventing a second drain from double-allocating it.
                continue;
            }
            // A request that fits the measured host but not the current
            // remainder owns the headroom when it becomes available. This
            // prevents a stream of smaller arrivals from starving it.
            if matches!(
                crate::resource_schedule::admit(&pending.node, &measured),
                Admit::Run
            ) {
                state.pending.push_front(pending);
                break;
            }
            state.pending.push_back(pending);
        }
    }
}

/// Process-wide production controller. All default DAG executions share one
/// reservation ledger, while tests can inject an isolated controller.
pub fn host_admission() -> LiveAdmission {
    static HOST: std::sync::OnceLock<LiveAdmission> = std::sync::OnceLock::new();
    HOST.get_or_init(LiveAdmission::host).clone()
}

fn block_reason(node: &DagNode, measured: &Resources) -> &'static str {
    if node.needs_model && !measured.model_ready {
        "model unavailable"
    } else if node.gpu_mem_gb > measured.gpu_mem_gb {
        "GPU memory unavailable"
    } else if node.subprocesses > measured.subprocesses {
        "subprocess capacity unavailable"
    } else {
        "resource request exceeds measured capacity"
    }
}

#[cfg(test)]
// Test module: panic-path macros are the assertion mechanism here; the
// mandate exemption applies to test code only.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dag::MissionDag;
    use crate::resource_schedule::DagNode;

    fn res(cpu: f64, mem: f64, gpu: f64, ready: bool) -> Resources {
        Resources {
            cpu,
            gpu_mem_gb: gpu,
            mem_gb: mem,
            subprocesses: 1,
            model_ready: ready,
            tool_grants: vec!["exec_command".to_string()],
        }
    }

    fn node(id: &str, cpu: f64, mem: f64, gpu: f64, model: bool) -> DagNode {
        DagNode {
            id: id.to_string(),
            cpu,
            gpu_mem_gb: gpu,
            mem_gb: mem,
            subprocesses: 1,
            needs_model: model,
            needs_tools: vec!["exec_command".to_string()],
        }
    }

    #[test]
    fn capacity_admission_measures_real_host() {
        // The production probe must reflect this machine, not constants.
        let r = measured();
        let parallelism = std::thread::available_parallelism()
            .map(|n| n.get() as f64)
            .unwrap_or(0.0);
        // CPU is either the measured parallelism or an explicit env pin —
        // never the old hardcoded 8.0 on a host reporting differently.
        if std::env::var("SUSI_ADMIT_CPU").is_err() {
            assert_eq!(r.cpu, parallelism.max(1.0));
        }
        assert!(r.cpu >= 1.0);
        assert!(r.mem_gb > 0.0);
    }

    #[test]
    fn capacity_admission_unmeasured_gpu_fails_closed() {
        // No GPU probe + no operator override → a GPU-hungry node queues
        // rather than overcommitting an assumed 16 GiB.
        let probe = FixedProbe(res(8.0, 64.0, 0.0, true));
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert(0, node("n0", 1.0, 0.0, 8.0, false));
        let (admitted, deferred, _) =
            admit_round(&probe, &[0], &reqs, MissionDag::default_resource_node);
        assert!(admitted.is_empty());
        assert_eq!(deferred, vec![0]);
    }

    #[test]
    fn capacity_admission_deferred_nodes_drain_next_round() {
        // First round admits what fits; the deferred node is retried when
        // capacity frees — it is not dropped from the dispatch.
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert(0, node("n0", 1.0, 0.0, 0.0, true));
        reqs.insert(1, node("n1", 1.0, 0.0, 0.0, true));
        let one_cpu = FixedProbe(res(1.0, 64.0, 0.0, true));
        let (admitted, deferred, _) =
            admit_round(&one_cpu, &[0, 1], &reqs, MissionDag::default_resource_node);
        assert_eq!(admitted, vec![0]);
        assert_eq!(deferred, vec![1]);
        // Round 2: the finished worker released its reservation.
        let (admitted, deferred, _) = admit_round(
            &one_cpu,
            &deferred,
            &reqs,
            MissionDag::default_resource_node,
        );
        assert_eq!(admitted, vec![1]);
        assert!(deferred.is_empty());
    }

    #[test]
    fn capacity_admission_memory_is_a_real_gate() {
        let tight = FixedProbe(res(8.0, 2.0, 0.0, true));
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert(0, node("n0", 0.5, 4.0, 0.0, false));
        let (admitted, deferred, _) =
            admit_round(&tight, &[0], &reqs, MissionDag::default_resource_node);
        assert!(admitted.is_empty());
        assert_eq!(deferred, vec![0]);
    }

    #[test]
    fn capacity_admission_model_readiness_is_measured() {
        // A host under the model floor must not claim model_ready.
        let weak = FixedProbe(res(4.0, 1.0, 0.0, false));
        let mut reqs = std::collections::BTreeMap::new();
        reqs.insert(0, node("n0", 0.5, 0.0, 0.0, true));
        let (admitted, _, _) = admit_round(&weak, &[0], &reqs, MissionDag::default_resource_node);
        assert!(admitted.is_empty());
        // The same host still runs tool-only nodes.
        reqs.insert(1, node("n1", 0.5, 0.0, 0.0, false));
        let (admitted, _, _) = admit_round(&weak, &[1], &reqs, MissionDag::default_resource_node);
        assert_eq!(admitted, vec![1]);
    }

    #[test]
    fn capacity_admission_override_pins_dag_snapshot() {
        // execute_dag honors the pinned capacity: a node that exceeds it
        // fails honestly instead of running against assumed resources.
        let mut dag = MissionDag::new("gate");
        dag.capacity = Some(res(0.5, 64.0, 0.0, true));
        let board = susi_gawd_agents::agents::MissionBlackboard::default();
        let (tx, _rx) = flume::unbounded();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("susi-cap-admit-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let err = dag
            .execute_dag(&dir, &board, &tx)
            .expect_err("undersized host must refuse the node");
        assert!(err.to_string().contains("no ready nodes admitted"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
