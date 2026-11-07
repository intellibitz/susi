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

impl ModelUseEvent {
    /// Verify a use event: it names a model (the scheduler skips an empty id).
    pub fn verify_event_invariants(&self) -> Result<(), String> {
        if self.model_id.trim().is_empty() {
            return Err("ModelUseEvent model_id must not be empty".to_string());
        }
        Ok(())
    }
}

impl PreloadPolicy {
    /// Verify a policy: at least one warm slot and a non-zero scoring window (a zero
    /// window drops every recency bonus and admits no use).
    pub fn verify_policy_invariants(&self) -> Result<(), String> {
        if self.max_warm == 0 {
            return Err("PreloadPolicy max_warm must be at least 1".to_string());
        }
        if self.window_secs == 0 {
            return Err("PreloadPolicy window_secs must be non-zero".to_string());
        }
        Ok(())
    }
}

impl PreloadCandidate {
    /// Verify a ranked candidate against the scoring contract of
    /// [`schedule_preload`]: a model id, at least one use inside the window, a last use
    /// time, and a score equal to the use count plus a recency bonus below `1.0`.
    pub fn verify_candidate_invariants(&self) -> Result<(), String> {
        if self.model_id.trim().is_empty() {
            return Err("PreloadCandidate model_id must not be empty".to_string());
        }
        if self.uses_in_window == 0 {
            return Err(format!(
                "PreloadCandidate `{}` has no use inside the window",
                self.model_id
            ));
        }
        if self.last_used_unix == 0 {
            return Err(format!(
                "PreloadCandidate `{}` has no last use time",
                self.model_id
            ));
        }
        let uses = self.uses_in_window as f64;
        if !self.score.is_finite() || self.score < uses || self.score > uses + 1.0 {
            return Err(format!(
                "PreloadCandidate `{}` score {} must be its {} use(s) plus a recency bonus in 0..=1",
                self.model_id, self.score, self.uses_in_window
            ));
        }
        Ok(())
    }
}

impl PreloadHysteresis {
    /// Verify a hysteresis band: a finite, non-negative margin.
    pub fn verify_hysteresis_invariants(&self) -> Result<(), String> {
        if !self.margin.is_finite() || self.margin < 0.0 {
            return Err(format!(
                "PreloadHysteresis margin must be finite and non-negative, got {}",
                self.margin
            ));
        }
        Ok(())
    }
}

/// Verify a preload schedule against `policy`: every candidate verifies, no model is
/// listed twice, the warm set fits `max_warm`, and candidates are ordered hottest first
/// (ties broken by model id, as [`schedule_preload`] orders them).
pub fn verify_preload_schedule(
    schedule: &[PreloadCandidate],
    policy: &PreloadPolicy,
) -> Result<(), String> {
    policy.verify_policy_invariants()?;
    if schedule.len() > policy.max_warm {
        return Err(format!(
            "preload schedule holds {} candidates, more than max_warm {}",
            schedule.len(),
            policy.max_warm
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for candidate in schedule {
        candidate.verify_candidate_invariants()?;
        if !seen.insert(candidate.model_id.as_str()) {
            return Err(format!(
                "preload schedule lists `{}` more than once",
                candidate.model_id
            ));
        }
    }
    for pair in schedule.windows(2) {
        let (hotter, cooler) = (&pair[0], &pair[1]);
        let in_order = hotter.score > cooler.score
            || (hotter.score == cooler.score && hotter.model_id < cooler.model_id);
        if !in_order {
            return Err(format!(
                "preload schedule is not ordered hottest first: `{}` ({}) before `{}` ({})",
                hotter.model_id, hotter.score, cooler.model_id, cooler.score
            ));
        }
    }
    Ok(())
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

    #[test]
    fn schedule_preload_handles_zero_window_secs_and_tie_breaks_by_model_id() {
        let events = vec![
            ModelUseEvent {
                model_id: "zeta".to_string(),
                used_unix: 100,
            },
            ModelUseEvent {
                model_id: "alpha".to_string(),
                used_unix: 100,
            },
        ];
        let policy = PreloadPolicy {
            max_warm: 1,
            window_secs: 0,
        };
        let ranked = schedule_preload(&events, &policy, Some(100));
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].model_id, "alpha");
    }

    #[test]
    fn preload_event_policy_candidate_and_schedule_invariant_verifiers_reject_bad_inputs() {
        let now = 1_000_000u64;
        let events = vec![
            ModelUseEvent {
                model_id: "alpha".to_string(),
                used_unix: now - 10,
            },
            ModelUseEvent {
                model_id: "alpha".to_string(),
                used_unix: now - 20,
            },
            ModelUseEvent {
                model_id: "beta".to_string(),
                used_unix: now - 30,
            },
        ];
        for event in &events {
            assert_eq!(event.verify_event_invariants(), Ok(()));
        }
        assert!(ModelUseEvent {
            model_id: " ".to_string(),
            used_unix: now,
        }
        .verify_event_invariants()
        .is_err());

        let policy = PreloadPolicy::default();
        assert_eq!(policy.verify_policy_invariants(), Ok(()));
        assert!(PreloadPolicy {
            max_warm: 0,
            window_secs: 60,
        }
        .verify_policy_invariants()
        .is_err());
        assert!(PreloadPolicy {
            max_warm: 1,
            window_secs: 0,
        }
        .verify_policy_invariants()
        .is_err());

        let schedule = schedule_preload(&events, &policy, Some(now));
        assert_eq!(verify_preload_schedule(&schedule, &policy), Ok(()));
        for candidate in &schedule {
            assert_eq!(candidate.verify_candidate_invariants(), Ok(()));
        }
        let mut inflated = schedule.clone();
        inflated[0].score = inflated[0].uses_in_window as f64 + 1.5;
        assert!(inflated[0].verify_candidate_invariants().is_err());
        let mut unused = schedule.clone();
        unused[0].uses_in_window = 0;
        assert!(unused[0].verify_candidate_invariants().is_err());
        let mut reversed = schedule.clone();
        reversed.reverse();
        assert!(verify_preload_schedule(&reversed, &policy).is_err());
        let mut duplicated = schedule.clone();
        duplicated.push(schedule[0].clone());
        assert!(verify_preload_schedule(&duplicated, &policy).is_err());
        let tight = PreloadPolicy {
            max_warm: 1,
            window_secs: policy.window_secs,
        };
        assert!(verify_preload_schedule(&schedule, &tight).is_err());

        assert_eq!(
            PreloadHysteresis { margin: 0.5 }.verify_hysteresis_invariants(),
            Ok(())
        );
        assert!(PreloadHysteresis { margin: -0.1 }
            .verify_hysteresis_invariants()
            .is_err());
        assert!(PreloadHysteresis { margin: f64::NAN }
            .verify_hysteresis_invariants()
            .is_err());
    }
}
