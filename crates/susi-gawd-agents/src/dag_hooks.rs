//! Post-swarm MissionDag seam so the agents leaf never depends on swarm.
//!
//! `GawdAgentFleet::dispatch_explosive_swarm` optionally appends DAG evidence
//! via a hook registered by `susi-gawd-swarm` at load time.

use crate::agents::MissionBlackboard;
use std::path::Path;
use std::sync::OnceLock;

pub type DagDispatchHook =
    fn(goal: &str, workspace: &Path, blackboard: &MissionBlackboard) -> Vec<(String, String)>;

static HOOK: OnceLock<DagDispatchHook> = OnceLock::new();

/// Register the swarm-owned MissionDag runner. Idempotent first-wins.
pub fn init(hook: DagDispatchHook) {
    let _ = HOOK.set(hook);
}

pub(crate) fn run(
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
) -> Vec<(String, String)> {
    match HOOK.get() {
        Some(hook) => hook(goal, workspace, blackboard),
        None => Vec::new(),
    }
}
