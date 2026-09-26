//! Swarm Composition Recommendation (Swarm OS Bullet 78)
//!
//! Recommends which cells to assemble for a new task by blending live
//! trust score (the same ranking `SwarmBlackboard::find_capable_cells`
//! already does) with historical success evidence: how many `Receipt`
//! pheromones (successful completions) each capable cell has deposited on
//! the task's topic. A cell with a strong track record on this exact kind
//! of work outranks an equally-trusted cell with no history of it.

use crate::susi_abi::swarm::{PheromoneKind, SwarmCellManifest, SwarmPheromone};

#[derive(Debug, Clone, PartialEq)]
pub struct CompositionCandidate {
    pub cell_id: String,
    pub trust_score: f32,
    pub historical_successes: usize,
}

/// Ranks `capable_cells` for `topic` using `topic_history` (the pheromones
/// already deposited under that topic), by trust score first and
/// historical `Receipt` count as the tiebreaker. Truncates to `team_size`.
pub fn recommend_composition(
    capable_cells: &[SwarmCellManifest],
    topic_history: &[SwarmPheromone],
    team_size: usize,
) -> Vec<CompositionCandidate> {
    let mut candidates: Vec<CompositionCandidate> = capable_cells
        .iter()
        .map(|cell| {
            let historical_successes = topic_history
                .iter()
                .filter(|p| p.kind == PheromoneKind::Receipt && p.emitter_id == cell.cell_id)
                .count();
            CompositionCandidate {
                cell_id: cell.cell_id.clone(),
                trust_score: cell.trust_score,
                historical_successes,
            }
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.trust_score
            .partial_cmp(&a.trust_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.historical_successes.cmp(&a.historical_successes))
    });
    candidates.truncate(team_size);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn manifest(cell_id: &str, trust_score: f32) -> SwarmCellManifest {
        SwarmCellManifest {
            cell_id: cell_id.to_string(),
            role: SwarmRole::ReflexCell,
            capabilities: Vec::new(),
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/test.sock".to_string(),
            trust_score,
            last_heartbeat: 0,
        }
    }

    fn receipt(emitter_id: &str, topic: &str) -> SwarmPheromone {
        SwarmPheromone {
            id: format!("{emitter_id}-{topic}"),
            topic: topic.to_string(),
            emitter_id: emitter_id.to_string(),
            kind: PheromoneKind::Receipt,
            intensity: 1.0,
            payload: serde_json::json!({}),
            ttl_ms: 0,
            deposited_at: 0,
        }
    }

    #[test]
    fn track_record_breaks_ties_between_equally_trusted_cells() {
        let cells = vec![manifest("no-history", 0.8), manifest("proven", 0.8)];
        let history = vec![
            receipt("proven", "intent.deploy"),
            receipt("proven", "intent.deploy"),
        ];

        let recommended = recommend_composition(&cells, &history, 2);
        assert_eq!(recommended[0].cell_id, "proven");
        assert_eq!(recommended[0].historical_successes, 2);
        assert_eq!(recommended[1].cell_id, "no-history");
        assert_eq!(recommended[1].historical_successes, 0);
    }

    #[test]
    fn trust_score_outranks_track_record() {
        let cells = vec![
            manifest("trusted", 0.9),
            manifest("proven-but-less-trusted", 0.3),
        ];
        let history = vec![receipt("proven-but-less-trusted", "intent.deploy")];

        let recommended = recommend_composition(&cells, &history, 2);
        assert_eq!(recommended[0].cell_id, "trusted");
    }

    #[test]
    fn team_size_truncates_the_recommendation() {
        let cells = vec![manifest("a", 0.9), manifest("b", 0.8), manifest("c", 0.7)];
        let recommended = recommend_composition(&cells, &[], 2);
        assert_eq!(recommended.len(), 2);
    }
}
