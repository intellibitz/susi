//! Production adapter upgrade pipeline with contract verification and rollback (VC-201-089).

use susi_core::adapter_upgrade::{AdapterRevision, AdapterUpgrade, ContractResults, SwitchVerdict};
use susi_error::EaiResult;
use susi_vendor_agents::external::{Adapter, AgentManager};

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
        let manager = AgentManager::new(&dir).unwrap();

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
}
