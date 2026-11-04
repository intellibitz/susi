//! Dispatch for remote adapters over the standard A2A protocol.

use super::{Adapter, AgentManager, RunRecord};
use susi_error::{eai_bail as bail, EaiResult as Result};

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::execute(manager, run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn refresh(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::refresh(manager, run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::cancel(run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn send(run: &RunRecord, message: &str) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::send(run, message);
    }
    bail!("not a cloud adapter")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::{CatalogKind, RunStatus};

    fn sample_run(workspace: &std::path::Path, adapter: Adapter) -> RunRecord {
        RunRecord {
            id: "run-local-1".into(),
            agent: "local-agent".into(),
            workspace: workspace.to_path_buf(),
            prompt: "verify cloud seam".into(),
            adapter,
            status: RunStatus::Running,
            created_at: 1,
            updated_at: 1,
            pid: None,
            remote_id: None,
            remote_url: None,
            exit_code: None,
            error: None,
        }
    }

    #[test]
    fn non_cloud_adapters_are_rejected_across_all_cloud_dispatch_entrypoints() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-cloud-seam-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let manager =
            AgentManager::with_config(&temp_dir, CatalogKind::Execution, temp_dir.join("config"))
                .unwrap();

        for adapter in [
            Adapter::Command {
                program: "echo".into(),
                args: vec!["hi".into()],
            },
            Adapter::Acp {
                program: "acp-bin".into(),
                args: vec![],
            },
        ] {
            let mut run = sample_run(&temp_dir, adapter);
            for result in [
                execute(&manager, &mut run),
                refresh(&manager, &mut run),
                cancel(&run),
                send(&run, "follow-up"),
            ] {
                let err = result.unwrap_err().to_string();
                assert!(
                    err.contains("not a cloud adapter"),
                    "unexpected error message: {err}"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
