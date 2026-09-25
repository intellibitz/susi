//! Hierarchical Resource Budgets (Swarm OS Bullets 29, 58)
//!
//! Swarms negotiate and enforce resource budgets (an abstract token unit
//! standing in for tokens, CPU-seconds, or money) across three levels —
//! cell, team, project — so a spend only succeeds if every level that has
//! a configured cap still has room. A level with no configured cap is
//! unconstrained rather than treated as zero. Debiting is all-or-nothing:
//! a request that would exceed any one level's cap is rejected without
//! partially spending at the others.

use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy)]
struct Budget {
    cap: u64,
    spent: u64,
}

impl Budget {
    fn remaining(&self) -> u64 {
        self.cap.saturating_sub(self.spent)
    }
}

pub struct HierarchicalBudget {
    cells: RwLock<HashMap<String, Budget>>,
    teams: RwLock<HashMap<String, Budget>>,
    projects: RwLock<HashMap<String, Budget>>,
}

impl Default for HierarchicalBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl HierarchicalBudget {
    pub fn new() -> Self {
        Self {
            cells: RwLock::new(HashMap::new()),
            teams: RwLock::new(HashMap::new()),
            projects: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_cell_cap(&self, cell_id: &str, cap: u64) {
        Self::set_cap(&self.cells, cell_id, cap);
    }

    pub fn set_team_cap(&self, team_id: &str, cap: u64) {
        Self::set_cap(&self.teams, team_id, cap);
    }

    pub fn set_project_cap(&self, project_id: &str, cap: u64) {
        Self::set_cap(&self.projects, project_id, cap);
    }

    fn set_cap(map: &RwLock<HashMap<String, Budget>>, key: &str, cap: u64) {
        let mut map = map.write().unwrap_or_else(|e| e.into_inner());
        let entry = map
            .entry(key.to_string())
            .or_insert(Budget { cap, spent: 0 });
        entry.cap = cap;
    }

    /// Negotiates a per-cell budget against its team's remaining pool
    /// (Bullet 29): `cell_id` requests `requested` units and is granted
    /// `min(requested, team's remaining headroom)` — an unconstrained team
    /// (no cap set) grants the request in full. The grant becomes the
    /// cell's own enforced cap. Returns the amount actually granted.
    pub fn negotiate_cell_budget(&self, team_id: &str, cell_id: &str, requested: u64) -> u64 {
        let team_remaining = self.team_remaining(team_id).unwrap_or(requested);
        let granted = requested.min(team_remaining);
        self.set_cell_cap(cell_id, granted);
        granted
    }

    /// Attempts to spend `amount` against `cell_id`, `team_id`, and
    /// `project_id` simultaneously. Succeeds only if every level with a
    /// configured cap has enough headroom; on success, debits all three
    /// at once.
    pub fn try_spend(
        &self,
        project_id: &str,
        team_id: &str,
        cell_id: &str,
        amount: u64,
    ) -> Result<(), String> {
        let mut cells = self.cells.write().unwrap_or_else(|e| e.into_inner());
        let mut teams = self.teams.write().unwrap_or_else(|e| e.into_inner());
        let mut projects = self.projects.write().unwrap_or_else(|e| e.into_inner());

        if cells.get(cell_id).is_some_and(|b| b.remaining() < amount) {
            return Err(format!("cell '{cell_id}' budget exhausted"));
        }
        if teams.get(team_id).is_some_and(|b| b.remaining() < amount) {
            return Err(format!("team '{team_id}' budget exhausted"));
        }
        if projects
            .get(project_id)
            .is_some_and(|b| b.remaining() < amount)
        {
            return Err(format!("project '{project_id}' budget exhausted"));
        }

        if let Some(b) = cells.get_mut(cell_id) {
            b.spent += amount;
        }
        if let Some(b) = teams.get_mut(team_id) {
            b.spent += amount;
        }
        if let Some(b) = projects.get_mut(project_id) {
            b.spent += amount;
        }
        Ok(())
    }

    pub fn cell_remaining(&self, cell_id: &str) -> Option<u64> {
        self.cells
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cell_id)
            .map(Budget::remaining)
    }

    pub fn team_remaining(&self, team_id: &str) -> Option<u64> {
        self.teams
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(team_id)
            .map(Budget::remaining)
    }

    pub fn project_remaining(&self, project_id: &str) -> Option<u64> {
        self.projects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(project_id)
            .map(Budget::remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconstrained_levels_never_block_a_spend() {
        let budget = HierarchicalBudget::new();
        // No caps configured anywhere.
        assert!(
            budget
                .try_spend("proj-a", "team-a", "cell-a", 1_000_000)
                .is_ok()
        );
    }

    #[test]
    fn any_exhausted_level_denies_the_whole_spend_atomically() {
        let budget = HierarchicalBudget::new();
        budget.set_cell_cap("cell-a", 100);
        budget.set_team_cap("team-a", 10); // tighter than the cell cap

        assert!(budget.try_spend("proj-a", "team-a", "cell-a", 50).is_err());
        // Denied at the team level must not have debited the cell either.
        assert_eq!(budget.cell_remaining("cell-a"), Some(100));
    }

    #[test]
    fn a_successful_spend_debits_every_configured_level() {
        let budget = HierarchicalBudget::new();
        budget.set_cell_cap("cell-a", 100);
        budget.set_team_cap("team-a", 200);
        budget.set_project_cap("proj-a", 500);

        assert!(budget.try_spend("proj-a", "team-a", "cell-a", 40).is_ok());
        assert_eq!(budget.cell_remaining("cell-a"), Some(60));
        assert_eq!(budget.team_remaining("team-a"), Some(160));
        assert_eq!(budget.project_remaining("proj-a"), Some(460));
    }

    #[test]
    fn negotiation_caps_the_request_at_the_teams_remaining_headroom() {
        let budget = HierarchicalBudget::new();
        budget.set_team_cap("team-a", 50);

        assert_eq!(budget.negotiate_cell_budget("team-a", "cell-a", 100), 50);
        assert_eq!(budget.cell_remaining("cell-a"), Some(50));
    }

    #[test]
    fn negotiation_against_an_unconstrained_team_grants_in_full() {
        let budget = HierarchicalBudget::new();
        assert_eq!(budget.negotiate_cell_budget("team-a", "cell-a", 30), 30);
    }
}
