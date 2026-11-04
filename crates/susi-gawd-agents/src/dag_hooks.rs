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
    run_with(HOOK.get().copied(), goal, workspace, blackboard)
}

fn run_with(
    hook: Option<DagDispatchHook>,
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
) -> Vec<(String, String)> {
    match hook {
        Some(hook) => hook(goal, workspace, blackboard),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::HighDensityContextStore;
    use std::sync::Arc;

    #[test]
    fn run_with_handles_unwired_and_wired_dag_hooks() {
        let bb: MissionBlackboard = Arc::new(HighDensityContextStore::new(64));
        bb.insert("seed".to_string(), "ok".to_string());
        let ws = Path::new("/tmp/dag-hook-ws");

        assert!(run_with(None, "verify", ws, &bb).is_empty());

        fn stub_hook(
            goal: &str,
            workspace: &Path,
            blackboard: &MissionBlackboard,
        ) -> Vec<(String, String)> {
            vec![(
                "MissionDag".to_string(),
                format!(
                    "goal={goal} ws={} seed={}",
                    workspace.display(),
                    blackboard.get("seed").unwrap_or_default()
                ),
            )]
        }

        let out = run_with(Some(stub_hook), "verify", ws, &bb);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "MissionDag");
        assert!(out[0].1.contains("goal=verify"));
        assert!(out[0].1.contains("seed=ok"));
    }
}
