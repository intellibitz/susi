//! Decide and build who acts on a served Tier-0/1 ACTION: line.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPlan {
    pub actor: String,
    pub action: String,
}

/// Parse an ACTION: line into an actor/action plan.
#[must_use]
pub fn plan_action(line: &str) -> Option<ActionPlan> {
    let rest = line.trim().strip_prefix("ACTION:")?.trim();
    let (actor, action) = rest.split_once(':').unwrap_or(("susi", rest));
    Some(ActionPlan {
        actor: actor.trim().to_string(),
        action: action.trim().to_string(),
    })
}

#[cfg(test)]
mod action_executor_tests {
    use super::*;

    #[test]
    fn action_executor_parses_tier_action_line() {
        let p = plan_action("ACTION: cursor: apply patch").unwrap();
        assert_eq!(p.actor, "cursor");
        assert_eq!(p.action, "apply patch");
        assert!(plan_action("not an action").is_none());
    }
}
