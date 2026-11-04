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

    #[test]
    fn run_with_forwards_empty_and_multi_entry_outputs_from_shared_blackboard() {
        let bb: MissionBlackboard = Arc::new(HighDensityContextStore::new(64));
        bb.insert("step_1".to_string(), "plan-complete".to_string());
        bb.insert("step_2".to_string(), "audit-clean".to_string());
        let ws = Path::new("/tmp/multi-dag-ws");

        fn empty_hook(
            _goal: &str,
            _workspace: &Path,
            _blackboard: &MissionBlackboard,
        ) -> Vec<(String, String)> {
            Vec::new()
        }

        fn multi_hook(
            goal: &str,
            workspace: &Path,
            blackboard: &MissionBlackboard,
        ) -> Vec<(String, String)> {
            vec![
                (
                    "Stage1".to_string(),
                    format!("{goal}:{}", blackboard.get("step_1").unwrap_or_default()),
                ),
                (
                    "Stage2".to_string(),
                    format!(
                        "{}:{}",
                        workspace.display(),
                        blackboard.get("step_2").unwrap_or_default()
                    ),
                ),
            ]
        }

        assert!(run_with(Some(empty_hook), "ship", ws, &bb).is_empty());

        let out = run_with(Some(multi_hook), "ship", ws, &bb);
        assert_eq!(
            out,
            vec![
                ("Stage1".to_string(), "ship:plan-complete".to_string()),
                (
                    "Stage2".to_string(),
                    format!("{}:audit-clean", ws.display())
                ),
            ]
        );
    }

    #[test]
    fn run_invokes_global_hook_safely_and_run_with_passes_goal_and_workspace() {
        let bb: MissionBlackboard = Arc::new(HighDensityContextStore::new(64));
        let ws = Path::new("/tmp/global-dag-ws");
        let _ = run("global-goal", ws, &bb);

        fn echo_hook(
            goal: &str,
            workspace: &Path,
            _blackboard: &MissionBlackboard,
        ) -> Vec<(String, String)> {
            vec![(goal.to_string(), workspace.display().to_string())]
        }

        let pairs = run_with(Some(echo_hook), "exact-goal", ws, &bb);
        assert_eq!(
            pairs,
            vec![("exact-goal".to_string(), ws.display().to_string())]
        );
    }

    #[test]
    fn hook_callback_mutates_and_reads_blackboard_entries_preserving_order() {
        let bb: MissionBlackboard = Arc::new(HighDensityContextStore::new(64));
        let ws = Path::new("/tmp/mutate-dag-ws");

        fn mutating_hook(
            goal: &str,
            _workspace: &Path,
            blackboard: &MissionBlackboard,
        ) -> Vec<(String, String)> {
            blackboard.insert("k1".to_string(), format!("{goal}-v1"));
            blackboard.insert("k2".to_string(), format!("{goal}-v2"));
            vec![
                ("k1".to_string(), blackboard.get("k1").unwrap_or_default()),
                ("k2".to_string(), blackboard.get("k2").unwrap_or_default()),
            ]
        }

        let out = run_with(Some(mutating_hook), "run", ws, &bb);
        assert_eq!(
            out,
            vec![
                ("k1".to_string(), "run-v1".to_string()),
                ("k2".to_string(), "run-v2".to_string()),
            ]
        );
        assert_eq!(bb.get("k1").as_deref(), Some("run-v1"));
        assert_eq!(bb.get("k2").as_deref(), Some("run-v2"));
    }

    #[test]
    fn run_with_handles_empty_goal_and_preserves_existing_blackboard_state_when_unwired() {
        let bb: MissionBlackboard = Arc::new(HighDensityContextStore::new(16));
        bb.insert("existing".to_string(), "kept".to_string());
        let out = run_with(None, "", Path::new("."), &bb);
        assert!(out.is_empty());
        assert_eq!(bb.get("existing").as_deref(), Some("kept"));
    }
}
