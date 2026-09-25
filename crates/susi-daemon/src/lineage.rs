//! Child cells (Swarm OS Bullet 15)
//!
//! A parent may spawn a child whose capabilities are the intersection of
//! what it asked for and what the parent itself holds, and whose budget
//! cannot exceed the parent's cap.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildCell {
    pub cell_id: String,
    pub parent_id: String,
    pub capabilities: Vec<String>,
    pub budget_units: u64,
}

/// Spawns a child. Requested capabilities the parent does not hold are
/// dropped, not granted. The budget is `min(requested, cap)`.
pub fn spawn_child(
    parent_id: &str,
    parent_capabilities: &[String],
    requested_capabilities: &[String],
    budget_cap: u64,
    requested_budget: u64,
) -> ChildCell {
    let mut capabilities: Vec<String> = requested_capabilities
        .iter()
        .filter(|cap| parent_capabilities.iter().any(|held| held == *cap))
        .cloned()
        .collect();
    capabilities.sort();
    capabilities.dedup();
    ChildCell {
        cell_id: format!("{parent_id}/child"),
        parent_id: parent_id.to_string(),
        capabilities,
        budget_units: requested_budget.min(budget_cap),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_cannot_exceed_parent_capabilities_or_budget() {
        let parent = vec!["infer".to_string(), "tool:git".to_string()];
        let child = spawn_child(
            "planner",
            &parent,
            &["tool:git".into(), "tool:prod-db".into()],
            10,
            40,
        );
        assert_eq!(child.capabilities, vec!["tool:git".to_string()]);
        assert_eq!(child.budget_units, 10);
        assert_eq!(child.parent_id, "planner");
    }
}
