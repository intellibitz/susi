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
    pub model_ready: bool,
    pub tool_grants: Vec<String>,
}

const fn unmeasured_mem() -> f64 {
    f64::MAX
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,
    pub cpu: f64,
    pub gpu_mem_gb: f64,
    /// System memory the node's work needs (GiB); `0` declares none.
    #[serde(default)]
    pub mem_gb: f64,
    pub needs_model: bool,
    pub needs_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admit {
    Run,
    Queue,
}

/// Admit runnable nodes without exceeding reservations.
#[must_use]
pub fn admit(node: &DagNode, free: &Resources) -> Admit {
    if node.needs_model && !free.model_ready {
        return Admit::Queue;
    }
    if node.cpu > free.cpu || node.gpu_mem_gb > free.gpu_mem_gb || node.mem_gb > free.mem_gb {
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
        model_ready: free.model_ready,
        tool_grants: free.tool_grants.clone(),
    }
}
