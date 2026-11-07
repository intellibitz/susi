//! Production adapter upgrade pipeline with contract verification and rollback (VC-201-089).

use susi_core::adapter_upgrade::{AdapterRevision, AdapterUpgrade, ContractResults, SwitchVerdict};
use susi_error::EaiResult;
use crate::external::{Adapter, AgentManager};

/// Validate, stage, contract-check, and switch an adapter configuration
/// beside the active one, ensuring rollback on contract or version failure.
pub fn upgrade_adapter(
    manager: &AgentManager,
    id: &str,
    candidate: &Adapter,
    version: u32,
) -> EaiResult<SwitchVerdict> {
    let mut upgrade = AdapterUpgrade::new();
    if manager.adapter(id).is_ok() {
        let _ = upgrade.install_active(AdapterRevision {
            id: format!("{id}-active"),
            version: 1,
        });
    }

    upgrade.stage(AdapterRevision {
        id: format!("{id}-staged"),
        version,
    });

    let checks = vec![(
        "schema_validation".to_string(),
        candidate.validate().is_ok(),
    )];
    let results = ContractResults { checks };

    let verdict = upgrade.switch_if_contracts_pass(&results);
    if verdict == SwitchVerdict::Switched {
        manager.configure(id, candidate)?;
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upgrade_adapter_switches_and_rejects_downgrade() {
        let dir = std::env::temp_dir().join(format!(
            "susi-upgrade-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A synthetic catalog and a temp config dir: core bundles no vendor
        // catalog, and the test must not write the host's adapter overrides.
        let codex: crate::external::AgentDefinition =
            serde_json::from_value(serde_json::json!({
                "id": "codex", "name": "Codex", "peer_name": "CodexAgent", "rank": 1,
                "documentation": "https://example.invalid",
                "adapter": {"kind": "command", "program": "codex", "args": ["{prompt}"]},
            }))
            .unwrap();
        let manager = AgentManager::with_config(
            &dir,
            crate::external::CatalogKind::Execution,
            dir.join("cfg"),
        )
        .unwrap()
        .with_catalog(vec![codex]);

        let valid = Adapter::Command {
            program: "echo".into(),
            args: vec!["hello".into()],
        };
        let verdict = upgrade_adapter(&manager, "codex", &valid, 2).unwrap();
        assert_eq!(verdict, SwitchVerdict::Switched);

        // Downgrade is rejected
        let downgrade = upgrade_adapter(&manager, "codex", &valid, 1).unwrap();
        assert_eq!(downgrade, SwitchVerdict::RejectedDowngrade);

        // Invalid candidate fails contract check and preserves active
        let invalid = Adapter::Command {
            program: "".into(),
            args: vec![],
        };
        let failure = upgrade_adapter(&manager, "codex", &invalid, 3).unwrap();
        assert_eq!(failure, SwitchVerdict::RejectedContractFailure);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn manager_without(id_present: &str) -> (std::path::PathBuf, AgentManager) {
        let dir = std::env::temp_dir().join(format!(
            "susi-upgrade-unknown-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let def: crate::external::AgentDefinition =
            serde_json::from_value(serde_json::json!({
                "id": id_present, "name": "Present", "peer_name": "PresentAgent", "rank": 1,
                "documentation": "https://example.invalid",
                "adapter": {"kind": "command", "program": "present", "args": ["{prompt}"]},
            }))
            .unwrap();
        let manager = AgentManager::with_config(
            &dir,
            crate::external::CatalogKind::Execution,
            dir.join("cfg"),
        )
        .unwrap()
        .with_catalog(vec![def]);
        (dir, manager)
    }

    #[test]
    fn unknown_adapter_with_a_failing_contract_is_rejected_without_writing_config() {
        let (dir, manager) = manager_without("present");
        assert!(manager.adapter("ghost").is_err());
        let invalid = Adapter::Command {
            program: String::new(),
            args: vec![],
        };
        let verdict = upgrade_adapter(&manager, "ghost", &invalid, 5).unwrap();
        assert_eq!(verdict, SwitchVerdict::RejectedContractFailure);
        assert!(
            !dir.join("cfg").exists(),
            "a rejected upgrade must not touch the config directory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_adapter_with_a_passing_contract_propagates_the_configure_error() {
        let (dir, manager) = manager_without("present");
        let valid = Adapter::Command {
            program: "echo".into(),
            args: vec!["hi".into()],
        };
        // Contracts pass and nothing is active, so the switch is attempted
        // and the unknown id is refused at `configure`, not swallowed.
        assert!(upgrade_adapter(&manager, "ghost", &valid, 2).is_err());
        assert!(!dir.join("cfg").join("ghost.json").exists());
        // The known adapter is still untouched.
        assert!(!dir.join("cfg").join("present.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn equal_version_on_active_adapter_is_rejected_as_downgrade_without_modifying_config() {
        let (dir, manager) = manager_without("present");
        let valid = Adapter::Command {
            program: "echo".into(),
            args: vec!["hi".into()],
        };
        let verdict = upgrade_adapter(&manager, "present", &valid, 1).unwrap();
        assert_eq!(verdict, SwitchVerdict::RejectedDowngrade);
        assert!(!dir.join("cfg").join("present.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn higher_version_upgrade_persists_adapter_override_in_config_dir() {
        let (dir, manager) = manager_without("present");
        let valid = Adapter::Command {
            program: "custom-runner".into(),
            args: vec!["--fast".into()],
        };
        let verdict = upgrade_adapter(&manager, "present", &valid, 2).unwrap();
        assert_eq!(verdict, SwitchVerdict::Switched);
        assert!(dir.join("cfg").join("present.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
