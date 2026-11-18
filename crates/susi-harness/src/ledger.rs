//! The append-only spend ledger.
//!
//! One row per dispatch attempt, JSON Lines, one writer (the supervisor). The
//! price in force at dispatch time is stored **with** the row, so a later
//! catalogue change cannot rewrite what a mission cost.

use crate::money::MicroUsd;
use crate::pricing::{ModelPrice, TokenUsage};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;
use susi_error::{EaiError, EaiResult};

static APPEND_LOCK: Mutex<()> = Mutex::new(());

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
    /// The model call completed at the provider and produced an answer.
    ///
    /// This is the gateway's outcome: the call happened and was priced, but
    /// whether the work passes acceptance is a later, separate verdict.
    Completed,
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
    /// True when the proxy served the answer from its cache, so no provider
    /// spend occurred. The tokens still describe the cached answer.
    #[serde(default)]
    pub cached: bool,
    /// True when `cost` is the figure the provider itself stated for the call
    /// (DeepInfra's `usage.estimated_cost`), not the catalogue's price applied
    /// to the tokens. The provider's bill is the fact the catalogue
    /// approximates, so where a provider states it, the row carries it and a
    /// stale price cannot misstate the spend. `price` still records the rate
    /// the call was admitted at.
    #[serde(default)]
    pub reported: bool,
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
            cached: false,
            reported: false,
            started_unix_ms: dispatch.started_unix_ms,
            ended_unix_ms: dispatch.ended_unix_ms,
            outcome: dispatch.outcome,
        }
    }

    /// Settle the row at the cost the provider stated for the call, when it
    /// stated one ([`crate::usage::provider_cost_from_json`]); `None` keeps
    /// the catalogue's figure. A cached row is settled before it is marked
    /// cached, so the cache's zero still wins.
    #[must_use]
    pub fn at_provider_cost(mut self, stated: Option<MicroUsd>) -> Self {
        if let Some(cost) = stated {
            self.cost = cost;
            self.reported = true;
        }
        self
    }

    /// Mark the row as served by the proxy's cache: the provider was not
    /// called, so the row costs nothing even though it records the tokens.
    #[must_use]
    pub fn as_cached(mut self) -> Self {
        self.cached = true;
        self.cost = MicroUsd::ZERO;
        self
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
    let _guard = APPEND_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| EaiError::io(format!("open ledger {}: {e}", path.display())))?;
    // O(1) tail byte check via SeekFrom::End(-1): if a prior crash left a
    // non-empty file whose last byte is not '\n', prepend '\n' so the new
    // JSON line starts cleanly on its own line rather than gluing onto a tear.
    let needs_leading_newline = match file.seek(SeekFrom::End(-1)) {
        Ok(_) => {
            let mut last = [0u8; 1];
            file.read_exact(&mut last).is_ok() && last[0] != b'\n'
        }
        Err(_) => false,
    };
    let mut row = Vec::with_capacity(line.len() + 2);
    if needs_leading_newline {
        row.push(b'\n');
    }
    row.extend_from_slice(line.as_bytes());
    row.push(b'\n');
    file.write_all(&row)
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

/// Every row of `path` that decodes, and how many non-blank lines did not.
///
/// For the readers that make decisions (the budget guard, the fleet's
/// evidence, a task's model order, the cockpit). The ledger is append-only and
/// many processes write it, so a line torn by a crash, or written by a newer
/// version, is possible; [`read_entries`] refuses the whole file on it, and its
/// callers fell back to an empty ledger, which dropped every charge (the
/// budget guard then saw nothing spent) and every lesson. This drops only the
/// unreadable lines and says how many there were. A missing ledger is empty.
#[must_use]
pub fn read_entries_lenient(path: &Path) -> (Vec<LedgerEntry>, usize) {
    read_rows_lenient(path)
}

/// Every line of `path` that decodes as a `T`, and how many non-blank lines
/// did not: [`read_entries_lenient`] for a reader that wants its own row type
/// (a `serde_json::Value` reads rows of any version).
///
/// The file is split into lines as bytes and each line is decoded on its own.
/// A tear can cut a multi-byte character, and the whole file then is not
/// UTF-8: read as one string it failed as a whole, and every row was dropped
/// with no line counted as skipped. A missing ledger is empty; any other read
/// failure is said on stderr, since it reads as nothing spent.
///
/// A torn row has no newline after it, so the next append lands on the same
/// line: a line that does not decode is tried again from each later `{`, and
/// a row found there counts while the fragment before it is skipped. Only the
/// glued row's own start can decode to the end of the line: a `{` inside the
/// fragment opens an object the fragment never closes, or one it closes
/// before more text.
#[must_use]
pub fn read_rows_lenient<T: serde::de::DeserializeOwned>(path: &Path) -> (Vec<T>, usize) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("⚠ ledger {}: cannot be read: {error}", path.display());
            }
            return (Vec::new(), 0);
        }
    };
    let mut rows = Vec::new();
    let mut skipped = 0;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty())
    {
        if let Ok(row) = serde_json::from_slice(line) {
            rows.push(row);
            continue;
        }
        skipped += 1;
        let glued = (1..line.len())
            .filter(|start| line[*start] == b'{')
            .find_map(|start| serde_json::from_slice(&line[start..]).ok());
        if let Some(row) = glued {
            rows.push(row);
        }
    }
    (rows, skipped)
}

/// [`read_entries_lenient`] that names the skipped lines on stderr, once per
/// read, so a damaged ledger is visible instead of silently thinner.
#[must_use]
pub fn read_entries_reporting(path: &Path) -> Vec<LedgerEntry> {
    let (entries, skipped) = read_entries_lenient(path);
    if skipped > 0 {
        eprintln!(
            "⚠ ledger {}: {skipped} line(s) could not be read and were skipped",
            path.display()
        );
    }
    entries
}

/// Sum the settled cost of every row, saturating rather than wrapping.
#[must_use]
pub fn total_cost(entries: &[LedgerEntry]) -> MicroUsd {
    entries
        .iter()
        .fold(MicroUsd::ZERO, |sum, entry| add(sum, entry.cost))
}

/// Sum the settled cost of the rows one mission charged to one task.
///
/// The gateway tags every row with the per-run key the executor set
/// (`susi:<mission>:<task>:<attempt>`), so filtering on both attributes
/// attributes a call to exactly one agent role. Rows for another mission or
/// another role in the same ledger cannot leak in.
#[must_use]
pub fn cost_for(entries: &[LedgerEntry], mission: &str, task: &str) -> MicroUsd {
    entries
        .iter()
        .filter(|entry| entry.mission == mission && entry.task == task)
        .fold(MicroUsd::ZERO, |sum, entry| add(sum, entry.cost))
}

/// Read the ledger and sum one mission's rows for one task.
///
/// An absent ledger has spent nothing. An unreadable line costs only itself
/// ([`read_entries_reporting`] skips it and says so on stderr): the strict
/// reader failed the whole file on one torn line, and every role's readback
/// then read $0 spent. The ledger itself is never written here.
#[must_use]
pub fn spent_for(path: &Path, mission: &str, task: &str) -> MicroUsd {
    cost_for(&read_entries_reporting(path), mission, task)
}

/// Add two amounts, saturating at the largest representable value.
fn add(total: MicroUsd, cost: MicroUsd) -> MicroUsd {
    total
        .checked_add(cost)
        .unwrap_or(MicroUsd::from_micros(u64::MAX))
}

/// Structural and chronological integrity report for a ledger file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LedgerIntegrityReport {
    /// Number of valid [`LedgerEntry`] rows decoded from the ledger.
    pub valid_entries: usize,
    /// Number of non-blank lines that could not be decoded as a full row.
    pub corrupted_lines: usize,
    /// Saturated sum of settled cost across all valid entries.
    pub total_cost: MicroUsd,
    /// Number of entries whose `ended_unix_ms < started_unix_ms` or whose
    /// `started_unix_ms` regressed relative to the preceding entry.
    pub non_monotonic_timestamps: usize,
}

impl LedgerIntegrityReport {
    /// True when the ledger has zero corrupted lines and zero non-monotonic timestamps.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.corrupted_lines == 0 && self.non_monotonic_timestamps == 0
    }
}

/// Audit a ledger file on disk and return its [`LedgerIntegrityReport`].
///
/// Missing files report zero entries and zero errors.
#[must_use]
pub fn verify_integrity(path: &Path) -> LedgerIntegrityReport {
    let (entries, corrupted_lines) = read_entries_lenient(path);
    let mut non_monotonic_timestamps = 0usize;
    let mut prev_started: Option<u64> = None;
    for entry in &entries {
        let inverted_span = entry.ended_unix_ms < entry.started_unix_ms;
        let regressed_start = prev_started.is_some_and(|prev| entry.started_unix_ms < prev);
        if inverted_span || regressed_start {
            non_monotonic_timestamps = non_monotonic_timestamps.saturating_add(1);
        }
        prev_started = Some(entry.started_unix_ms);
    }
    LedgerIntegrityReport {
        valid_entries: entries.len(),
        corrupted_lines,
        total_cost: total_cost(&entries),
        non_monotonic_timestamps,
    }
}

#[cfg(test)]
#[path = "tests/ledger.rs"]
mod tests;
