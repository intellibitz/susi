//! Tool browse and browser capability (Swarm OS Bullets 44 and 45)
//!
//! Cells browse the cards their MAC grants allow. Invoking `browser`
//! returns the registered cell that declares `tool:browser`. With no such
//! cell, invoke fails closed instead of pretending a browser ran.

use crate::security::{CapabilityGrant, CapabilityPolicy};
use susi_abi::swarm::SwarmCellManifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCard {
    pub name: String,
    pub requires_capability: String,
}

pub fn builtin_cards() -> Vec<ToolCard> {
    vec![
        ToolCard {
            name: "git".into(),
            requires_capability: "tool:git".into(),
        },
        ToolCard {
            name: "filesystem".into(),
            requires_capability: "tool:filesystem".into(),
        },
        ToolCard {
            name: "browser".into(),
            requires_capability: "tool:browser".into(),
        },
    ]
}

pub fn browse(cards: &[ToolCard], policy: &CapabilityPolicy) -> Vec<ToolCard> {
    cards
        .iter()
        .filter(|card| {
            policy
                .grants()
                .iter()
                .any(|grant| grant_covers(grant, &card.requires_capability))
        })
        .cloned()
        .collect()
}

fn grant_covers(grant: &CapabilityGrant, required: &str) -> bool {
    if grant.capability == required {
        return true;
    }
    grant
        .capability
        .strip_suffix('*')
        .is_some_and(|prefix| required.starts_with(prefix))
}

/// The cell that declares `tool:browser`, if one is registered.
pub fn invoke_browser(cells: &[SwarmCellManifest]) -> Result<String, String> {
    cells
        .iter()
        .find(|cell| cell.capabilities.iter().any(|cap| cap == "tool:browser"))
        .map(|cell| cell.cell_id.clone())
        .ok_or_else(|| "no browser driver cell registered".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    #[test]
    fn browse_hides_cards_the_policy_does_not_grant_and_browser_invoke_needs_a_driver() {
        let policy = CapabilityPolicy::new(
            "cell",
            vec![CapabilityGrant {
                capability: "tool:git".into(),
                scope: None,
                ephemeral: false,
            }],
        );
        let visible: Vec<String> = browse(&builtin_cards(), &policy)
            .into_iter()
            .map(|card| card.name)
            .collect();
        assert_eq!(visible, vec!["git".to_string()]);

        assert!(invoke_browser(&[]).is_err());
        let driver = SwarmCellManifest {
            cell_id: "browser-cell".into(),
            role: SwarmRole::ToolDriver,
            capabilities: vec!["tool:browser".into()],
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/browser.sock".into(),
            trust_score: 0.4,
            last_heartbeat: 0,
        };
        assert_eq!(invoke_browser(&[driver]).unwrap(), "browser-cell");
    }
}
