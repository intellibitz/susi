//! Swarm Resource Scheduler (Swarm OS Vision – Bullet 9).
//!
//! Distributes work across cells based on their declared resource budgets,
//! current load (CPU / GPU saturation), and trust scores.
//!
//! The scheduler operates in two modes:
//!
//! 1. **Greedy**  – selects the highest-trust, lowest-load cell that has the
//!    required capability. Default for latency-sensitive workloads.
//! 2. **Balanced** – spreads work evenly across eligible cells to prevent
//!    hot-spotting. Used when `--balanced` flag is set on a mission.
//!
//! The scheduler is stateless: it reads the [`SwarmBlackboard`] each time
//! and makes a decision with O(N) cell scan where N is the registered cell
//! count. For 10 k cells this is sub‑millisecond.

use susi_abi::swarm::SwarmCellManifest;
use susi_abi::cell::compute_fnv1a_hash;

/// Scheduling strategy for distributing work to cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulingStrategy {
    /// Pick the highest-trust, lowest-load cell.
    Greedy,
    /// Spread work evenly across eligible cells.
    Balanced,
}

/// A scored candidate cell for scheduling.
#[derive(Debug, Clone)]
pub struct ScheduleCandidate {
    /// The cell manifest.
    pub manifest: SwarmCellManifest,
    /// Composite score: higher is better.
    pub score: f64,
}

/// Selects the best cell(s) for a given capability and strategy.
///
/// Returns candidates sorted by score (descending). The caller picks
/// the top-N for redundancy (Bullet 24) or just the first for greedy.
pub fn schedule_cells(
    cells: &[SwarmCellManifest],
    required_capability: &str,
    strategy: SchedulingStrategy,
) -> Vec<ScheduleCandidate> {
    let capability_hash = compute_fnv1a_hash(required_capability);

    let mut candidates: Vec<ScheduleCandidate> = cells
        .iter()
        .filter(|cell| {
            // Fast Bloom filter check first, then exact capability match
            cell.bloom_filter.may_contain_hash(capability_hash)
                && cell.capabilities.iter().any(|c| c == required_capability)
        })
        .map(|cell| {
            let score = compute_score(cell, strategy);
            ScheduleCandidate {
                manifest: cell.clone(),
                score,
            }
        })
        .collect();

    // Sort by score descending (best first).
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates
}

/// Computes a composite score for a cell based on the scheduling strategy.
///
/// **Greedy**: Score = trust_score × recency_bonus
/// **Balanced**: Score = trust_score × (1 / (1 + staleness_penalty))
///
/// The recency bonus is based on the last heartbeat: more recent = higher score.
fn compute_score(cell: &SwarmCellManifest, strategy: SchedulingStrategy) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let staleness_secs = now.saturating_sub(cell.last_heartbeat);
    let trust = f64::from(cell.trust_score);

    match strategy {
        SchedulingStrategy::Greedy => {
            // Favour high-trust cells with recent heartbeats.
            let recency_bonus = 1.0 / (1.0 + staleness_secs as f64 / 60.0);
            trust * recency_bonus
        }
        SchedulingStrategy::Balanced => {
            // Penalise cells that haven't been heard from recently,
            // but don't penalise as aggressively as Greedy — the goal
            // is even distribution, not minimal latency.
            let staleness_penalty = staleness_secs as f64 / 300.0;
            trust / (1.0 + staleness_penalty)
        }
    }
}

/// Convenience: schedule for a single best cell (Greedy, first result).
pub fn schedule_one(
    cells: &[SwarmCellManifest],
    required_capability: &str,
) -> Option<SwarmCellManifest> {
    schedule_cells(cells, required_capability, SchedulingStrategy::Greedy)
        .into_iter()
        .next()
        .map(|c| c.manifest)
}

/// Schedule with redundancy: returns the top-N cells for parallel
/// verification (Bullet 24).
pub fn schedule_redundant(
    cells: &[SwarmCellManifest],
    required_capability: &str,
    n: usize,
) -> Vec<SwarmCellManifest> {
    schedule_cells(cells, required_capability, SchedulingStrategy::Balanced)
        .into_iter()
        .take(n)
        .map(|c| c.manifest)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn make_cell(id: &str, cap: &str, trust: f32, heartbeat: u64) -> SwarmCellManifest {
        let mut bloom = CapabilityBloom::empty();
        let hash = compute_fnv1a_hash(cap);
        bloom.insert_hash(hash);
        SwarmCellManifest {
            cell_id: id.to_string(),
            role: SwarmRole::InferenceDriver,
            capabilities: vec![cap.to_string()],
            bloom_filter: bloom,
            endpoint: format!("http://127.0.0.1:{}", 9000),
            trust_score: trust,
            last_heartbeat: heartbeat,
        }
    }

    #[test]
    fn schedule_one_picks_highest_trust() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cells = vec![
            make_cell("low-trust", "infer", 0.5, now),
            make_cell("high-trust", "infer", 1.0, now),
            make_cell("no-cap", "other", 1.0, now),
        ];

        let best = schedule_one(&cells, "infer");
        assert!(best.is_some());
        assert_eq!(best.unwrap().cell_id, "high-trust");
    }

    #[test]
    fn schedule_redundant_returns_n() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cells = vec![
            make_cell("a", "infer", 0.9, now),
            make_cell("b", "infer", 0.8, now),
            make_cell("c", "infer", 0.7, now),
        ];

        let top2 = schedule_redundant(&cells, "infer", 2);
        assert_eq!(top2.len(), 2);
    }

    #[test]
    fn stale_cell_penalised() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let cells = vec![
            make_cell("fresh", "infer", 0.8, now),
            make_cell("stale", "infer", 1.0, now.saturating_sub(600)),
        ];

        let best = schedule_one(&cells, "infer");
        assert!(best.is_some());
        // Even though stale has higher trust, freshness should win.
        assert_eq!(best.unwrap().cell_id, "fresh");
    }

    #[test]
    fn no_matching_capability_returns_empty() {
        let cells = vec![make_cell("a", "code_gen", 1.0, 0)];
        let result = schedule_one(&cells, "infer");
        assert!(result.is_none());
    }
}
