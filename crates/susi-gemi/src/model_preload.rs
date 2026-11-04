//! Warm-model preload scheduler from usage patterns (VC-201-047).
//!
//! Ranks models by recent use frequency/recency and returns a bounded
//! preload order — the hot set stays warm before the next mission asks.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// One observed use of a model (path or id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUseEvent {
    pub model_id: String,
    pub used_unix: u64,
}

/// Policy knobs for the warm set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreloadPolicy {
    /// Maximum models to keep warm.
    pub max_warm: usize,
    /// Ignore uses older than this many seconds when scoring.
    pub window_secs: u64,
}

impl Default for PreloadPolicy {
    fn default() -> Self {
        Self {
            max_warm: 3,
            window_secs: 24 * 60 * 60,
        }
    }
}

/// Ranked preload candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreloadCandidate {
    pub model_id: String,
    pub score: f64,
    pub uses_in_window: u64,
    pub last_used_unix: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Score = uses_in_window + recency_bonus (0..1). Higher is hotter.
#[must_use]
pub fn schedule_preload(
    events: &[ModelUseEvent],
    policy: &PreloadPolicy,
    now_unix_secs: Option<u64>,
) -> Vec<PreloadCandidate> {
    let now = now_unix_secs.unwrap_or_else(now_unix);
    let cutoff = now.saturating_sub(policy.window_secs);
    let mut agg: HashMap<String, (u64, u64)> = HashMap::new();
    for e in events {
        if e.used_unix < cutoff || e.model_id.is_empty() {
            continue;
        }
        let entry = agg.entry(e.model_id.clone()).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.max(e.used_unix);
    }
    let mut out: Vec<PreloadCandidate> = agg
        .into_iter()
        .map(|(model_id, (uses, last))| {
            let age = now.saturating_sub(last) as f64;
            let recency = if policy.window_secs == 0 {
                0.0
            } else {
                1.0 - (age / policy.window_secs as f64).clamp(0.0, 1.0)
            };
            PreloadCandidate {
                model_id,
                score: uses as f64 + recency,
                uses_in_window: uses,
                last_used_unix: last,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    if out.len() > policy.max_warm {
        out.truncate(policy.max_warm);
    }
    out
}

/// Hysteresis band for preload eviction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreloadHysteresis {
    /// A challenger must beat an incumbent's score by at least this margin
    /// before it is allowed to evict the incumbent from the warm set.
    pub margin: f64,
}

/// Damped preload schedule: a model already held warm stays warm unless a
/// challenger exceeds its score by `hysteresis.margin`. Tied or alternating
/// load therefore keeps the incumbent instead of flapping between two models
/// on every tick, which avoids repeated load/evict thrashing.
#[must_use]
pub fn schedule_preload_damped(
    events: &[ModelUseEvent],
    policy: &PreloadPolicy,
    incumbent_ids: &[String],
    hysteresis: &PreloadHysteresis,
    now_unix_secs: Option<u64>,
) -> Vec<PreloadCandidate> {
    let full_policy = PreloadPolicy {
        max_warm: usize::MAX,
        window_secs: policy.window_secs,
    };
    let ranked = schedule_preload(events, &full_policy, now_unix_secs);
    let incumbent: std::collections::HashSet<&str> =
        incumbent_ids.iter().map(String::as_str).collect();

    let mut warm: Vec<PreloadCandidate> = Vec::with_capacity(policy.max_warm);
    // Incumbents that are still ranked keep their slot first.
    for c in &ranked {
        if incumbent.contains(c.model_id.as_str()) && warm.len() < policy.max_warm {
            warm.push(c.clone());
        }
    }
    // Fill free slots with the strongest challengers, then only evict an
    // incumbent when a challenger clears the hysteresis margin.
    for c in &ranked {
        if incumbent.contains(c.model_id.as_str()) {
            continue;
        }
        if warm.len() < policy.max_warm {
            warm.push(c.clone());
            continue;
        }
        if let Some(weakest) = warm
            .iter()
            .enumerate()
            .filter(|(_, w)| incumbent.contains(w.model_id.as_str()))
            .min_by(|(_, a), (_, b)| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
        {
            if c.score > warm[weakest].score + hysteresis.margin {
                warm[weakest] = c.clone();
            }
        }
    }
    warm.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    warm
}

/// Cold-start count over a mixed workload: uses of a model that is not in the
/// warm set. A smaller count means the warm set avoided a reload.
#[must_use]
pub fn cold_starts(events: &[ModelUseEvent], warm_ids: &[String]) -> usize {
    let warm: std::collections::HashSet<&str> = warm_ids.iter().map(String::as_str).collect();
    events
        .iter()
        .filter(|e| !warm.contains(e.model_id.as_str()))
        .count()
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn default_preload_policy_uses_three_warm_models_and_twenty_four_hour_window() {
        let policy = PreloadPolicy::default();
        assert_eq!(policy.max_warm, 3);
        assert_eq!(policy.window_secs, 24 * 60 * 60);
    }

    #[test]
    fn schedule_preload_ranks_by_frequency_and_recency_while_filtering_expired_and_empty() {
        let policy = PreloadPolicy {
            max_warm: 2,
            window_secs: 100,
        };
        let events = vec![
            ModelUseEvent {
                model_id: "expired-model".to_string(),
                used_unix: 800,
            },
            ModelUseEvent {
                model_id: String::new(),
                used_unix: 995,
            },
            ModelUseEvent {
                model_id: "model-a".to_string(),
                used_unix: 950,
            },
            ModelUseEvent {
                model_id: "model-a".to_string(),
                used_unix: 990,
            },
            ModelUseEvent {
                model_id: "model-b".to_string(),
                used_unix: 980,
            },
            ModelUseEvent {
                model_id: "model-c".to_string(),
                used_unix: 910,
            },
        ];
        let ranked = schedule_preload(&events, &policy, Some(1_000));
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].model_id, "model-a");
        assert_eq!(ranked[0].uses_in_window, 2);
        assert_eq!(ranked[0].last_used_unix, 990);
        assert_eq!(ranked[1].model_id, "model-b");
    }

    #[test]
    fn schedule_preload_damped_retains_incumbent_until_challenger_clears_hysteresis_margin() {
        let policy = PreloadPolicy {
            max_warm: 1,
            window_secs: 100,
        };
        let events = vec![
            ModelUseEvent {
                model_id: "incumbent".to_string(),
                used_unix: 920,
            },
            ModelUseEvent {
                model_id: "challenger".to_string(),
                used_unix: 990,
            },
        ];
        let incumbents = vec!["incumbent".to_string()];

        let retained = schedule_preload_damped(
            &events,
            &policy,
            &incumbents,
            &PreloadHysteresis { margin: 1.0 },
            Some(1_000),
        );
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].model_id, "incumbent");

        let evicted = schedule_preload_damped(
            &events,
            &policy,
            &incumbents,
            &PreloadHysteresis { margin: 0.5 },
            Some(1_000),
        );
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].model_id, "challenger");
    }

    #[test]
    fn cold_starts_counts_events_whose_model_id_is_absent_from_warm_set() {
        let events = vec![
            ModelUseEvent {
                model_id: "warm-1".to_string(),
                used_unix: 10,
            },
            ModelUseEvent {
                model_id: "cold-1".to_string(),
                used_unix: 20,
            },
            ModelUseEvent {
                model_id: "warm-2".to_string(),
                used_unix: 30,
            },
            ModelUseEvent {
                model_id: "cold-2".to_string(),
                used_unix: 40,
            },
        ];
        let warm = vec!["warm-1".to_string(), "warm-2".to_string()];
        assert_eq!(cold_starts(&events, &warm), 2);
        assert_eq!(
            cold_starts(
                &events,
                &[
                    "warm-1".to_string(),
                    "warm-2".to_string(),
                    "cold-1".to_string(),
                    "cold-2".to_string()
                ]
            ),
            0
        );
    }
}
