//! Scheduled brain scout (VC-202-003 / T-DEEPSEEK-107).
//!
//! Scouting once at start-up is not scouting: keys expire, prices change,
//! and the model that led last month may not lead today. The daemon's
//! maintenance tick runs [`run_scheduled`] on the `brain_scout` subject of
//! the shared probe scheduler — jittered, budgeted, offline-gated like the
//! ecosystem probe. Each run probes every registered provider through
//! [`crate::scout_probe`], records the verified outcomes as brain evidence,
//! snapshots the leader per task class, diffs that snapshot against the
//! previous run, and appends the run to an append-only journal — the
//! previous ranking kept as evidence, so drift is reviewable, not just
//! logged.

use crate::engines::brain::{self, TaskClass};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default cadence: every six hours. Override with
/// `SUSI_BRAIN_SCOUT_INTERVAL_SECS`; the probe set is bounded and cheap by
/// design, so this is safe to run on a daily-or-better rhythm forever.
pub fn interval_secs() -> u64 {
    std::env::var("SUSI_BRAIN_SCOUT_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(21_600)
}

/// One provider's outcome in a scout run — the evidence drift is computed
/// from and the journal keeps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoutProviderResult {
    pub provider: String,
    pub verified: usize,
    pub probes: usize,
    pub transport_failures: usize,
}

impl From<&crate::scout_probe::ScoutReport> for ScoutProviderResult {
    fn from(r: &crate::scout_probe::ScoutReport) -> Self {
        Self {
            provider: r.provider.clone(),
            verified: r.verified,
            probes: r.probes,
            transport_failures: r.transport_failures,
        }
    }
}

/// One scout run: when it happened, who led each task class afterwards,
/// and how every probed provider scored. Appended to the journal as one
/// JSON line per run — the previous ranking is evidence, not memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoutRun {
    pub unix: u64,
    /// Task-class label → the leading *fit* provider (unfit never leads).
    pub leaders: BTreeMap<String, Option<String>>,
    pub providers: Vec<ScoutProviderResult>,
}

/// What changed between two scout runs — the report the scheduler owes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScoutDrift {
    /// The leading provider for a task class changed since the last run.
    LeaderChanged {
        class: String,
        was: Option<String>,
        now: Option<String>,
    },
    /// A provider that answered at least one probe last run now fails
    /// every transport — a key expired, a quota died, an endpoint went away.
    ProviderDied { provider: String },
}

/// Leaders per task class from the live brain evidence: the first *fit*
/// provider in each class's ranking. An unfit provider never appears as a
/// leader even if its score sorts first (Mandate 56).
#[must_use]
pub fn current_leaders(names: &[String]) -> BTreeMap<String, Option<String>> {
    let names = names.to_vec();
    TaskClass::ALL
        .iter()
        .map(|class| {
            let leader = brain::rank(&names, *class)
                .into_iter()
                .find(|r| !r.unfit)
                .map(|r| r.provider);
            (class.label().to_string(), leader)
        })
        .collect()
}

/// The drift between the previous run and this one.
#[must_use]
pub fn drift_between(prev: &ScoutRun, cur: &ScoutRun) -> Vec<ScoutDrift> {
    let mut out = Vec::new();
    for (class, now_leader) in &cur.leaders {
        let was = prev.leaders.get(class).cloned().unwrap_or(None);
        if &was != now_leader {
            out.push(ScoutDrift::LeaderChanged {
                class: class.clone(),
                was,
                now: now_leader.clone(),
            });
        }
    }
    for cur_p in &cur.providers {
        let Some(prev_p) = prev.providers.iter().find(|p| p.provider == cur_p.provider) else {
            continue;
        };
        let was_alive = prev_p.transport_failures < prev_p.probes;
        let now_dead = cur_p.probes > 0 && cur_p.transport_failures == cur_p.probes;
        if was_alive && now_dead {
            out.push(ScoutDrift::ProviderDied {
                provider: cur_p.provider.clone(),
            });
        }
    }
    out
}

/// Read the last recorded run — the previous ranking the next run diffs
/// against. A missing or corrupt journal is just "no previous evidence".
#[must_use]
pub fn load_last(journal: &Path) -> Option<ScoutRun> {
    let text = std::fs::read_to_string(journal).ok()?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ScoutRun>(l).ok())
        .next_back()
}

/// Append one run to the journal — one JSON line per scout run.
pub fn append(journal: &Path, run: &ScoutRun) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = journal.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(run)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal)?;
    writeln!(f, "{line}")
}

/// One full scheduled sweep — the production path the daemon's maintenance
/// tick calls. Probes every registered provider through the real registry
/// and arbiter ([`crate::scout_probe::probe_registered`]), records the
/// verified outcomes into the brain, snapshots leaders, diffs against the
/// journal's last run, and appends this run as the new evidence. Returns
/// the run and whatever drift it produced.
pub fn run_scheduled(now: u64, journal: &Path) -> (ScoutRun, Vec<ScoutDrift>) {
    crate::engines::http_provider::register_configured_cloud_endpoints(
        crate::susi_core::registry::CapabilityRegistry::global(),
    );
    let names = crate::susi_core::registry::CapabilityRegistry::global().list_providers();
    let mut providers = Vec::with_capacity(names.len());
    for name in &names {
        let report = crate::scout_probe::probe_registered(name)
            .unwrap_or_else(|e| crate::scout_probe::ScoutReport::transport_dead(name, e));
        crate::scout_probe::record_outcomes(&report);
        providers.push(ScoutProviderResult::from(&report));
    }
    let run = ScoutRun {
        unix: now,
        leaders: current_leaders(&names),
        providers,
    };
    let drift = load_last(journal)
        .map(|prev| drift_between(&prev, &run))
        .unwrap_or_default();
    if let Err(e) = append(journal, &run) {
        tracing::warn!("brain scout journal append failed: {e}");
    }
    (run, drift)
}

/// Where the journal lives by default — the daemon passes its home-bound
/// path explicitly; this is for surfaces and tests that want the shared one.
#[must_use]
pub fn default_journal_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("brain_scout_runs.jsonl")
}
