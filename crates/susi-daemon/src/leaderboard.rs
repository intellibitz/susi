//! Cell Leaderboard (Swarm OS Bullet 79)
//!
//! Ranks registered Swarm Cells by trust score — the one leaderboard
//! metric `SwarmCellManifest` actually carries (evidence-derived, per
//! `find_capable_cells`'s existing ranking in `blackboard.rs`). Speed and
//! cost-efficiency ranking need per-task telemetry this manifest doesn't
//! carry yet, so this stays honestly scoped to trust rather than faking
//! the other columns.

use susi_abi::swarm::SwarmCellManifest;

/// One leaderboard row: a cell's rank (1-based) alongside its manifest.
#[derive(Debug, Clone)]
pub struct LeaderboardEntry {
    pub rank: usize,
    pub cell_id: String,
    pub trust_score: f32,
    pub last_heartbeat: u64,
}

/// Ranks `cells` by descending trust score, breaking ties by the more
/// recently active cell (higher `last_heartbeat`).
pub fn rank_by_trust(cells: &[SwarmCellManifest]) -> Vec<LeaderboardEntry> {
    let mut ranked: Vec<&SwarmCellManifest> = cells.iter().collect();
    ranked.sort_by(|a, b| {
        b.trust_score
            .partial_cmp(&a.trust_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.last_heartbeat.cmp(&a.last_heartbeat))
    });
    ranked
        .into_iter()
        .enumerate()
        .map(|(i, cell)| LeaderboardEntry {
            rank: i + 1,
            cell_id: cell.cell_id.clone(),
            trust_score: cell.trust_score,
            last_heartbeat: cell.last_heartbeat,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn manifest(cell_id: &str, trust_score: f32, last_heartbeat: u64) -> SwarmCellManifest {
        SwarmCellManifest {
            cell_id: cell_id.to_string(),
            role: SwarmRole::ReflexCell,
            capabilities: Vec::new(),
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/test.sock".to_string(),
            trust_score,
            last_heartbeat,
        }
    }

    #[test]
    fn ranks_highest_trust_first() {
        let cells = vec![
            manifest("low", 0.2, 1),
            manifest("high", 0.9, 1),
            manifest("mid", 0.5, 1),
        ];
        let board = rank_by_trust(&cells);
        assert_eq!(
            board.iter().map(|e| e.cell_id.as_str()).collect::<Vec<_>>(),
            vec!["high", "mid", "low"]
        );
        assert_eq!(board[0].rank, 1);
        assert_eq!(board[2].rank, 3);
    }

    #[test]
    fn ties_break_on_more_recent_heartbeat() {
        let cells = vec![manifest("stale", 0.7, 100), manifest("fresh", 0.7, 500)];
        let board = rank_by_trust(&cells);
        assert_eq!(board[0].cell_id, "fresh");
    }
}
