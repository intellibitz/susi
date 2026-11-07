//! Schedule DAG nodes by live resource constraints (VC-201-024).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu: f64,
    pub gpu_mem_gb: f64,
    /// Host system memory (GiB). `f64::MAX` means the platform does not
    /// expose a readable figure — the dimension is unmeasurable, so
    /// admission does not gate on it (never silently invent a number).
    #[serde(default = "unmeasured_mem")]
    pub mem_gb: f64,
    /// Available subprocess slots. A measured zero means no child process
    /// may be launched; it is not an invitation to assume a default.
    #[serde(default)]
    pub subprocesses: u32,
    pub model_ready: bool,
    pub tool_grants: Vec<String>,
}

const fn unmeasured_mem() -> f64 {
    f64::MAX
}

impl Resources {
    /// Verifies non-negative finite capacity bounds and non-empty tool grant strings.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `cpu`, `gpu_mem_gb`, or `mem_gb` is
    /// non-finite or negative, or if any entry in `tool_grants` is blank.
    pub fn verify_resources_invariants(&self) -> Result<(), String> {
        if !self.cpu.is_finite() || self.cpu < 0.0 {
            return Err("Resources cpu must be finite and >= 0.0".to_string());
        }
        if !self.gpu_mem_gb.is_finite() || self.gpu_mem_gb < 0.0 {
            return Err("Resources gpu_mem_gb must be finite and >= 0.0".to_string());
        }
        if !self.mem_gb.is_finite() || self.mem_gb < 0.0 {
            return Err("Resources mem_gb must be finite and >= 0.0".to_string());
        }
        if self.tool_grants.iter().any(|g| g.trim().is_empty()) {
            return Err("Resources tool_grants must not contain blank entries".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,
    pub cpu: f64,
    pub gpu_mem_gb: f64,
    /// System memory the node's work needs (GiB); `0` declares none.
    #[serde(default)]
    pub mem_gb: f64,
    /// Child processes the node needs while it runs.
    #[serde(default = "default_subprocesses")]
    pub subprocesses: u32,
    pub needs_model: bool,
    pub needs_tools: Vec<String>,
}

impl DagNode {
    /// Verifies node identity, non-negative finite resource requests, and non-empty tool requirements.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string if `id` is blank, if `cpu`, `gpu_mem_gb`,
    /// or `mem_gb` is non-finite or negative, or if any entry in `needs_tools` is blank.
    pub fn verify_node_invariants(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("DagNode id must not be blank".to_string());
        }
        if !self.cpu.is_finite() || self.cpu < 0.0 {
            return Err("DagNode cpu must be finite and >= 0.0".to_string());
        }
        if !self.gpu_mem_gb.is_finite() || self.gpu_mem_gb < 0.0 {
            return Err("DagNode gpu_mem_gb must be finite and >= 0.0".to_string());
        }
        if !self.mem_gb.is_finite() || self.mem_gb < 0.0 {
            return Err("DagNode mem_gb must be finite and >= 0.0".to_string());
        }
        if self.needs_tools.iter().any(|t| t.trim().is_empty()) {
            return Err("DagNode needs_tools must not contain blank entries".to_string());
        }
        Ok(())
    }
}

const fn default_subprocesses() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admit {
    Run,
    Queue,
}

/// Admit runnable nodes without exceeding reservations.
#[must_use]
pub fn admit(node: &DagNode, free: &Resources) -> Admit {
    if !node.cpu.is_finite()
        || node.cpu < 0.0
        || !node.gpu_mem_gb.is_finite()
        || node.gpu_mem_gb < 0.0
        || !node.mem_gb.is_finite()
        || node.mem_gb < 0.0
        || !free.cpu.is_finite()
        || !free.gpu_mem_gb.is_finite()
        || !free.mem_gb.is_finite()
    {
        return Admit::Queue;
    }
    if node.needs_model && !free.model_ready {
        return Admit::Queue;
    }
    if node.cpu > free.cpu
        || node.gpu_mem_gb > free.gpu_mem_gb
        || node.mem_gb > free.mem_gb
        || node.subprocesses > free.subprocesses
    {
        return Admit::Queue;
    }
    if !node
        .needs_tools
        .iter()
        .all(|t| free.tool_grants.iter().any(|g| g == t))
    {
        return Admit::Queue;
    }
    Admit::Run
}

/// Remaining free resources after admitting a run.
#[must_use]
pub fn reserve(free: &Resources, node: &DagNode) -> Resources {
    Resources {
        cpu: (free.cpu - node.cpu).max(0.0),
        gpu_mem_gb: (free.gpu_mem_gb - node.gpu_mem_gb).max(0.0),
        mem_gb: (free.mem_gb - node.mem_gb).max(0.0),
        subprocesses: free.subprocesses.saturating_sub(node.subprocesses),
        model_ready: free.model_ready,
        tool_grants: free.tool_grants.clone(),
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn sample_free() -> Resources {
        Resources {
            cpu: 4.0,
            gpu_mem_gb: 8.0,
            mem_gb: 16.0,
            subprocesses: 2,
            model_ready: true,
            tool_grants: vec!["read_file".into(), "exec_command".into()],
        }
    }

    fn sample_node() -> DagNode {
        DagNode {
            id: "n1".into(),
            cpu: 2.0,
            gpu_mem_gb: 4.0,
            mem_gb: 8.0,
            subprocesses: 1,
            needs_model: true,
            needs_tools: vec!["read_file".into()],
        }
    }

    #[test]
    fn admit_returns_run_when_all_resource_model_and_tool_constraints_fit() {
        let free = sample_free();
        let node = sample_node();
        assert_eq!(admit(&node, &free), Admit::Run);
    }

    #[test]
    fn admit_queues_on_non_finite_or_negative_resource_requests() {
        let free = sample_free();
        let mut neg_cpu = sample_node();
        neg_cpu.cpu = -0.1;
        assert_eq!(admit(&neg_cpu, &free), Admit::Queue);

        let mut nan_gpu = sample_node();
        nan_gpu.gpu_mem_gb = f64::NAN;
        assert_eq!(admit(&nan_gpu, &free), Admit::Queue);
    }

    #[test]
    fn admit_queues_when_model_not_ready_or_tool_grant_or_capacity_missing() {
        let mut free = sample_free();
        let mut node = sample_node();

        free.model_ready = false;
        assert_eq!(admit(&node, &free), Admit::Queue);
        free.model_ready = true;

        node.needs_tools.push("ungranted_tool".into());
        assert_eq!(admit(&node, &free), Admit::Queue);
        node.needs_tools.pop();

        node.subprocesses = 3;
        assert_eq!(admit(&node, &free), Admit::Queue);
    }

    #[test]
    fn reserve_subtracts_used_capacity_and_clamps_at_zero() {
        let free = sample_free();
        let mut node = sample_node();
        node.cpu = 10.0;
        node.subprocesses = 5;
        let rem = reserve(&free, &node);
        assert!(rem.cpu.abs() < f64::EPSILON);
        assert!((rem.gpu_mem_gb - 4.0).abs() < f64::EPSILON);
        assert!((rem.mem_gb - 8.0).abs() < f64::EPSILON);
        assert_eq!(rem.subprocesses, 0);
        assert_eq!(rem.tool_grants, free.tool_grants);
    }

    #[test]
    fn resources_and_dag_node_serde_defaults_populate_unmeasured_mem_and_single_subprocess() {
        let res: Resources = serde_json::from_str(
            r#"{"cpu":2.0,"gpu_mem_gb":0.0,"model_ready":false,"tool_grants":[]}"#,
        )
        .unwrap();
        assert_eq!(res.mem_gb, f64::MAX);
        assert_eq!(res.subprocesses, 0);

        let node: DagNode = serde_json::from_str(
            r#"{"id":"n-def","cpu":1.0,"gpu_mem_gb":0.0,"needs_model":false,"needs_tools":[]}"#,
        )
        .unwrap();
        assert!(node.mem_gb.abs() < f64::EPSILON);
        assert_eq!(node.subprocesses, 1);
    }

    #[test]
    fn resources_and_dag_node_invariants_verify_and_reject_invalid_bounds() {
        let free = sample_free();
        assert!(free.verify_resources_invariants().is_ok());
        let mut bad_free = free;
        bad_free.cpu = -1.0;
        assert!(bad_free.verify_resources_invariants().is_err());

        let node = sample_node();
        assert!(node.verify_node_invariants().is_ok());
        let mut bad_node = node;
        bad_node.id = "  ".into();
        assert!(bad_node.verify_node_invariants().is_err());
    }
}
