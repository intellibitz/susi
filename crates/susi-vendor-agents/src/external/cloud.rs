//! Dispatch for remote adapters over the standard A2A protocol.

use super::{Adapter, AgentManager, RunRecord};
use susi_error::{eai_bail as bail, EaiResult as Result};

pub(super) fn execute(_manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        bail!("A2A external agent plug is in susi-commercial-apps");
    }
    bail!("not a cloud adapter")
}

pub(super) fn refresh(_manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        bail!("A2A external agent plug is in susi-commercial-apps");
    }
    bail!("not a cloud adapter")
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        bail!("A2A external agent plug is in susi-commercial-apps");
    }
    bail!("not a cloud adapter")
}

pub(super) fn send(run: &RunRecord, _message: &str) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        bail!("A2A external agent plug is in susi-commercial-apps");
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

    #[test]
    fn rejected_non_cloud_dispatch_leaves_run_record_unmutated() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-cloud-unmutated-test-{}-{}",
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
            assert!(execute(&manager, &mut run).is_err());
            assert!(refresh(&manager, &mut run).is_err());
            assert_eq!(run.status, RunStatus::Running);
            assert_eq!(run.id, "run-local-1");
            assert!(run.remote_id.is_none());
            assert!(run.error.is_none());
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn cancel_and_send_reject_command_and_acp_adapters_with_consistent_error() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-cloud-cancel-send-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();

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
            let run = sample_run(&temp_dir, adapter);
            let cancel_err = cancel(&run).unwrap_err().to_string();
            let send_err = send(&run, "ping").unwrap_err().to_string();
            assert!(cancel_err.contains("not a cloud adapter"), "{cancel_err}");
            assert!(send_err.contains("not a cloud adapter"), "{send_err}");
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn sample_run_preserves_workspace_prompt_and_timestamps_when_cloud_entrypoints_fail() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-cloud-preserve-fields-test-{}-{}",
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

        let mut run = sample_run(
            &temp_dir,
            Adapter::Command {
                program: "echo".into(),
                args: vec![],
            },
        );
        assert!(execute(&manager, &mut run).is_err());
        assert!(refresh(&manager, &mut run).is_err());
        assert_eq!(run.workspace, temp_dir);
        assert_eq!(run.prompt, "verify cloud seam");
        assert_eq!(run.created_at, 1);
        assert_eq!(run.updated_at, 1);
        assert!(run.pid.is_none());
        assert!(run.exit_code.is_none());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn non_cloud_dispatch_rejects_empty_and_multitoken_messages_identically() {
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-cloud-msg-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let run = sample_run(
            &temp_dir,
            Adapter::Command {
                program: "echo".into(),
                args: vec![],
            },
        );
        for msg in ["", "single", "multi token follow-up"] {
            let err = send(&run, msg).unwrap_err().to_string();
            assert!(err.contains("not a cloud adapter"));
        }
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
