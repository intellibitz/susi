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
