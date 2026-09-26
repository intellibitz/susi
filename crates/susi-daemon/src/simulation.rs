//! Simulation mode (Swarm OS Bullet 65)
//!
//! Assigns synthetic tasks to the highest-trust cell whose bloom filter
//! may contain the task's capability hash. The input roster is not mutated.

use crate::susi_abi::swarm::SwarmCellManifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticTask {
    pub name: String,
    pub capability_hash: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimResult {
    pub task: String,
    pub assigned_cell: Option<String>,
    pub tokens: u64,
}

pub fn run(cells: &[SwarmCellManifest], tasks: &[SyntheticTask]) -> Vec<SimResult> {
    tasks
        .iter()
        .map(|task| {
            let mut capable: Vec<&SwarmCellManifest> = cells
                .iter()
                .filter(|cell| cell.bloom_filter.may_contain_hash(task.capability_hash))
                .collect();
            capable.sort_by(|left, right| {
                right
                    .trust_score
                    .partial_cmp(&left.trust_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            SimResult {
                task: task.name.clone(),
                assigned_cell: capable.first().map(|cell| cell.cell_id.clone()),
                tokens: task.tokens,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn cell(id: &str, trust: f32, hash: Option<u64>) -> SwarmCellManifest {
        let mut bloom_filter = CapabilityBloom::default();
        if let Some(hash) = hash {
            bloom_filter.insert_hash(hash);
        }
        SwarmCellManifest {
            cell_id: id.into(),
            role: SwarmRole::ReflexCell,
            capabilities: Vec::new(),
            bloom_filter,
            endpoint: "ipc:///tmp/test.sock".into(),
            trust_score: trust,
            last_heartbeat: 0,
        }
    }

    #[test]
    fn the_higher_trust_capable_cell_is_assigned_and_the_roster_is_unchanged() {
        const HASH: u64 = 9;
        let cells = vec![cell("low", 0.2, Some(HASH)), cell("high", 0.9, Some(HASH))];
        let results = run(
            &cells,
            &[SyntheticTask {
                name: "bench".into(),
                capability_hash: HASH,
                tokens: 12,
            }],
        );
        assert_eq!(results[0].assigned_cell.as_deref(), Some("high"));
        assert_eq!(results[0].tokens, 12);
        assert_eq!(cells.len(), 2);
    }
}
