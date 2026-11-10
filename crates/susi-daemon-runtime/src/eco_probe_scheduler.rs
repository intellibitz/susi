//! Budgeted probe scheduler (VC-201-088 / T-CLAUDE-340).
//!
//! Capability probes (see `susi_vendor_models::eco_live_probe`) run on a
//! schedule: each subject has a refresh interval, a deterministic jitter so
//! fleets do not stampede the same vendor, a per-subject request budget, and
//! a privacy/consent gate. The scheduler is pure — given `now`, last-run
//! timestamps and the environment flags it returns a [`Decision`] per
//! subject; the caller performs (or skips) the actual probe.

use std::collections::BTreeMap;

/// Per-subject probing policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// Profile or vendor id the probe refreshes.
    pub id: String,
    /// Minimum seconds between probe runs.
    pub interval_secs: u64,
    /// Request budget per run (a plan never exceeds it).
    pub budget: usize,
    /// Probing this subject needs explicit consent (sends anything at all).
    pub needs_consent: bool,
}

/// Environment inputs the scheduler is not allowed to learn itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Conditions {
    /// No network — every probe is skipped.
    pub offline: bool,
    /// User consent for outbound probes.
    pub consented: bool,
}

/// What the scheduler decided for one subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run now; the attached jitter already applied to `due_at`.
    Run { due_at: u64 },
    /// Not yet due; `due_at` is when it becomes eligible.
    Wait { due_at: u64 },
    /// Skipped by conditions or consent; probing may resume later.
    Skip(&'static str),
}

/// Deterministic jitter in `[0, interval_secs/4]` derived from the subject id
/// (FNV-1a — stable across restarts, no RNG state to keep).
#[must_use]
pub fn jitter_for(id: &str, interval_secs: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h % (interval_secs / 4 + 1)
}

/// The scheduler: last-run timestamps plus a request ledger.
#[derive(Debug, Default)]
pub struct ProbeScheduler {
    last_run: BTreeMap<String, u64>,
    spent: BTreeMap<String, usize>,
}

impl ProbeScheduler {
    /// When a subject next becomes eligible: a never-probed subject is due
    /// now; afterwards `last + interval + jitter`.
    #[must_use]
    fn due_at(&self, s: &Subject) -> u64 {
        match self.last_run.get(&s.id) {
            None => 0,
            Some(last) => last
                .saturating_add(s.interval_secs)
                .saturating_add(jitter_for(&s.id, s.interval_secs)),
        }
    }

    /// Decide for one subject at `now` under `cond`.
    #[must_use]
    pub fn decide(&self, s: &Subject, now: u64, cond: Conditions) -> Decision {
        if cond.offline {
            return Decision::Skip("offline");
        }
        if s.needs_consent && !cond.consented {
            return Decision::Skip("unconsented");
        }
        let due = self.due_at(s);
        if now >= due {
            Decision::Run { due_at: due }
        } else {
            Decision::Wait { due_at: due }
        }
    }

    /// Record that a subject ran at `now` spending `requests` (clamped to
    /// its budget — the ledger never reports overspending as success).
    pub fn record_run(&mut self, s: &Subject, now: u64, requests: usize) {
        self.last_run.insert(s.id.clone(), now);
        *self.spent.entry(s.id.clone()).or_insert(0) += requests.min(s.budget);
    }

    /// Total requests spent on a subject so far.
    #[must_use]
    pub fn spent(&self, id: &str) -> usize {
        self.spent.get(id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(id: &str) -> Subject {
        Subject {
            id: id.into(),
            interval_secs: 3600,
            budget: 4,
            needs_consent: true,
        }
    }

    #[test]
    fn eco_probe_scheduler_first_run_is_due_immediately() {
        let sched = ProbeScheduler::default();
        let d = sched.decide(
            &subject("openai"),
            10_000,
            Conditions {
                offline: false,
                consented: true,
            },
        );
        assert!(matches!(d, Decision::Run { .. }));
    }

    #[test]
    fn eco_probe_scheduler_waits_until_interval_plus_jitter() {
        let mut sched = ProbeScheduler::default();
        let s = subject("openai");
        sched.record_run(&s, 1000, 2);
        let due = 1000 + 3600 + jitter_for("openai", 3600);
        let cond = Conditions {
            offline: false,
            consented: true,
        };
        assert_eq!(
            sched.decide(&s, due - 1, cond),
            Decision::Wait { due_at: due }
        );
        assert_eq!(sched.decide(&s, due, cond), Decision::Run { due_at: due });
    }

    #[test]
    fn eco_probe_scheduler_skips_offline_and_unconsented() {
        let sched = ProbeScheduler::default();
        let s = subject("openai");
        assert_eq!(
            sched.decide(
                &s,
                0,
                Conditions {
                    offline: true,
                    consented: true
                }
            ),
            Decision::Skip("offline")
        );
        assert_eq!(
            sched.decide(
                &s,
                0,
                Conditions {
                    offline: false,
                    consented: false
                }
            ),
            Decision::Skip("unconsented")
        );
        // a subject that needs no consent still runs without it
        let mut free = subject("local");
        free.needs_consent = false;
        assert!(matches!(
            sched.decide(
                &free,
                0,
                Conditions {
                    offline: false,
                    consented: false
                }
            ),
            Decision::Run { .. }
        ));
    }

    #[test]
    fn eco_probe_scheduler_jitter_is_deterministic_and_bounded() {
        let a = jitter_for("openai", 3600);
        assert_eq!(a, jitter_for("openai", 3600));
        assert!(a <= 900);
        // different subjects generally get different jitter
        assert_ne!(jitter_for("openai", 3600), jitter_for("anthropic", 3600));
    }

    #[test]
    fn eco_probe_scheduler_budget_clamps_the_ledger() {
        let mut sched = ProbeScheduler::default();
        let s = subject("openai");
        sched.record_run(&s, 0, 99);
        assert_eq!(sched.spent("openai"), 4); // clamped to budget
    }
}
