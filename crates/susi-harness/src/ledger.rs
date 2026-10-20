//! The append-only spend ledger.
//!
//! One row per dispatch attempt, JSON Lines, one writer (the supervisor). The
//! price in force at dispatch time is stored **with** the row, so a later
//! catalogue change cannot rewrite what a mission cost.

use crate::money::MicroUsd;
use crate::pricing::{ModelPrice, TokenUsage};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

/// Why an attempt failed at the provider boundary.
///
/// The class decides what happens next: `RateLimited` and `Timeout` retry the
/// same model, `NoCredit` pauses the mission for a top-up, and the rest move
/// the task down the capability ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// The provider's rate limit was hit.
    RateLimited,
    /// The call timed out.
    Timeout,
    /// The key has no credit left.
    NoCredit,
    /// The model answered, but not in a form the caller could use.
    MalformedOutput,
    /// The executor's tool call failed.
    ToolError,
    /// Anything not classified above.
    Other,
}

/// How one attempt ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The attempt produced work that passed acceptance.
    Accepted,
    /// The model or provider failed.
    ModelFailed {
        /// Typed failure class.
        class: FailureClass,
    },
    /// The work was produced but rejected (tests red, scope violated, …).
    Rejected {
        /// Human-readable reason, no code.
        reason: String,
    },
    /// The mission or task was cancelled before an answer arrived.
    Cancelled,
}

/// Everything one dispatch is about to spend and did produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    /// Mission id.
    pub mission: String,
    /// Task id.
    pub task: String,
    /// 1-based attempt number within the task.
    pub attempt: u32,
    /// Model id as the provider names it.
    pub model: String,
    /// Provider id (`deepseek`, `gemini`, …).
    pub provider: String,
    /// Tokens consumed.
    pub usage: TokenUsage,
    /// Price in force at dispatch time.
    pub price: ModelPrice,
    /// Milliseconds since the Unix epoch when the call started.
    pub started_unix_ms: u64,
    /// Milliseconds since the Unix epoch when the call ended.
    pub ended_unix_ms: u64,
    /// How the attempt ended.
    pub outcome: Outcome,
}

/// One settled row of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Mission id.
    pub mission: String,
    /// Task id.
    pub task: String,
    /// 1-based attempt number within the task.
    pub attempt: u32,
    /// Model id.
    pub model: String,
    /// Provider id.
    pub provider: String,
    /// Tokens consumed.
    pub usage: TokenUsage,
    /// Price in force at dispatch time.
    pub price: ModelPrice,
    /// Settled cost.
    pub cost: MicroUsd,
    /// Start, milliseconds since the Unix epoch.
    pub started_unix_ms: u64,
    /// End, milliseconds since the Unix epoch.
    pub ended_unix_ms: u64,
    /// How the attempt ended.
    pub outcome: Outcome,
}

impl LedgerEntry {
    /// Price a dispatch and build its ledger row.
    #[must_use]
    pub fn settle(dispatch: Dispatch) -> Self {
        let cost = dispatch.price.cost(dispatch.usage);
        Self {
            mission: dispatch.mission,
            task: dispatch.task,
            attempt: dispatch.attempt,
            model: dispatch.model,
            provider: dispatch.provider,
            usage: dispatch.usage,
            price: dispatch.price,
            cost,
            started_unix_ms: dispatch.started_unix_ms,
            ended_unix_ms: dispatch.ended_unix_ms,
            outcome: dispatch.outcome,
        }
    }

    /// Wall-clock latency of the attempt, saturating on a non-monotonic clock.
    #[must_use]
    pub fn latency_ms(&self) -> u64 {
        self.ended_unix_ms.saturating_sub(self.started_unix_ms)
    }
}

/// Append one settled row as a JSON line.
///
/// The caller is the single writer (the supervisor); concurrent writers would
/// interleave lines, which is why the contract is one accountant.
///
/// # Errors
///
/// Returns `EaiError::io` when the row cannot be encoded or the file cannot be
/// opened or written.
pub fn append_entry(path: &Path, entry: &LedgerEntry) -> EaiResult<()> {
    let line = serde_json::to_string(entry)
        .map_err(|e| EaiError::io(format!("encode ledger entry: {e}")))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| EaiError::io(format!("open ledger {}: {e}", path.display())))?;
    writeln!(file, "{line}")
        .map_err(|e| EaiError::io(format!("append ledger {}: {e}", path.display())))
}

/// Read every settled row, in file order.
///
/// Blank lines are ignored; a malformed line is an error naming the line, not
/// a silently dropped charge.
///
/// # Errors
///
/// Returns `EaiError::io` when the file cannot be read or a line does not
/// decode as a [`LedgerEntry`].
pub fn read_entries(path: &Path) -> EaiResult<Vec<LedgerEntry>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| EaiError::io(format!("read ledger {}: {e}", path.display())))?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry = serde_json::from_str(line).map_err(|e| {
            EaiError::io(format!("ledger {} line {}: {e}", path.display(), index + 1))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Sum the settled cost of every row, saturating rather than wrapping.
#[must_use]
pub fn total_cost(entries: &[LedgerEntry]) -> MicroUsd {
    entries.iter().fold(MicroUsd::ZERO, |sum, entry| {
        sum.checked_add(entry.cost)
            .unwrap_or(MicroUsd::from_micros(u64::MAX))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{ModelPrice, TokenUsage};

    fn price() -> ModelPrice {
        ModelPrice {
            input_miss: MicroUsd::from_micros(150_000),
            input_hit: MicroUsd::from_micros(3_000),
            output: MicroUsd::from_micros(600_000),
        }
    }

    fn entry(task: &str, outcome: Outcome) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: "VC-202-025".into(),
            task: task.into(),
            attempt: 1,
            model: "deepseek-flash".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(3_000_000, 2_400_000, 200_000),
            price: price(),
            started_unix_ms: 1_000,
            ended_unix_ms: 2_500,
            outcome,
        })
    }

    #[test]
    fn settle_prices_the_dispatch_and_keeps_the_rate() {
        let settled = entry("T-HARNESSPHASE0-1", Outcome::Accepted);
        assert_eq!(settled.cost, MicroUsd::from_micros(217_200));
        assert_eq!(settled.price, price());
        assert_eq!(settled.latency_ms(), 1_500);
    }

    #[test]
    fn ledger_round_trips_as_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let accepted = entry("T-1", Outcome::Accepted);
        let failed = entry(
            "T-2",
            Outcome::ModelFailed {
                class: FailureClass::Timeout,
            },
        );
        append_entry(&path, &accepted).unwrap();
        append_entry(&path, &failed).unwrap();

        let read = read_entries(&path).unwrap();
        assert_eq!(read, vec![accepted, failed]);
        assert_eq!(total_cost(&read), MicroUsd::from_micros(217_200 + 217_200));
    }

    #[test]
    fn a_malformed_line_is_an_error_not_a_dropped_charge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        std::fs::write(&path, "{\"mission\":\"m\"}\nnot json\n").unwrap();
        let err = read_entries(&path).unwrap_err();
        assert!(err.to_string().contains("line 1"));
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        assert!(read_entries(&path).is_err());
    }
}
