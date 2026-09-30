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

use crate::resource_schedule::{DagNode, Resources};

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
    Resources {
        cpu,
        gpu_mem_gb,
        mem_gb,
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

#[cfg(test)]
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
