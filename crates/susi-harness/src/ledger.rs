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
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| EaiError::io(format!("open ledger {}: {e}", path.display())))?;
    // One buffer, one write: several agents append to the same ledger, and a
    // line split across two writes could interleave with another writer's line.
    //
    // A row torn by a crash has no newline after it, so this row can land on
    // its line; the lenient reader recovers it ([`read_rows_lenient`]). The
    // writer does not look at the file's last byte first: that is a read of
    // the ledger on every append, and on Windows the read handle had the
    // growing file scanned on each one (eight concurrent appenders went from
    // under a second to over four minutes).
    let mut row = line.into_bytes();
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

    /// Eight writers, each appending its own rows to one ledger at once. The
    /// file must end with every row, each one whole: the appends are single
    /// writes, so no line is split or spliced with another writer's line.
    #[test]
    fn concurrent_appenders_never_interleave_ledger_rows() {
        let dir =
            std::env::temp_dir().join(format!("susi-ledger-concurrency-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.jsonl");
        let _ = std::fs::remove_file(&path);
        let writers = 8;
        let rows_each = 200;
        std::thread::scope(|scope| {
            for writer in 0..writers {
                let path = path.clone();
                scope.spawn(move || {
                    for row in 0..rows_each {
                        let task = format!("w{writer}-r{row}");
                        append_entry(&path, &entry(&task, Outcome::Completed)).unwrap();
                    }
                });
            }
        });
        let entries = read_entries(&path).expect("every line must decode");
        assert_eq!(entries.len(), writers * rows_each);
        let mut seen: Vec<&str> = entries.iter().map(|e| e.task.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), writers * rows_each, "no row lost or duplicated");
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A row whose cost is exactly `micros`: one million input tokens at that
    /// per-million miss rate and no output.
    fn charged(mission: &str, task: &str, micros: u64) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: mission.into(),
            task: task.into(),
            attempt: 1,
            model: "deepseek-flash".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(micros),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: Outcome::Completed,
        })
    }

    #[test]
    fn settle_prices_the_dispatch_and_keeps_the_rate() {
        let settled = entry("T-HARNESSPHASE0-1", Outcome::Accepted);
        assert_eq!(settled.cost, MicroUsd::from_micros(217_200));
        assert_eq!(settled.price, price());
        assert_eq!(settled.latency_ms(), 1_500);
        assert!(!settled.cached);
    }

    #[test]
    fn a_cached_row_costs_nothing_and_says_so() {
        let cached = entry("T-HARNESSPHASE0-1", Outcome::Accepted).as_cached();
        assert!(cached.cached);
        assert_eq!(cached.cost, MicroUsd::ZERO);
        assert_eq!(cached.usage, TokenUsage::new(3_000_000, 2_400_000, 200_000));
        let json = serde_json::to_string(&cached).unwrap();
        assert!(json.contains("\"cached\":true"));
    }

    #[test]
    fn an_old_row_without_the_cached_field_reads_as_not_cached() {
        let mut value = serde_json::to_value(entry("T-1", Outcome::Accepted)).unwrap();
        value.as_object_mut().unwrap().remove("cached");
        let parsed: LedgerEntry = serde_json::from_value(value).unwrap();
        assert!(!parsed.cached);
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
    fn cost_for_picks_only_this_missions_role() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        append_entry(&path, &charged("live-1", "planner", 120)).unwrap();
        append_entry(&path, &charged("live-1", "planner", 80)).unwrap();
        append_entry(&path, &charged("live-1", "coder-1", 900)).unwrap();
        append_entry(&path, &charged("live-1", "reviewer", 40)).unwrap();
        append_entry(&path, &charged("other-1", "planner", 700)).unwrap();

        let entries = read_entries(&path).unwrap();
        // Both planner rows and no other role: 120 + 80.
        assert_eq!(
            cost_for(&entries, "live-1", "planner"),
            MicroUsd::from_micros(200)
        );
        // The same role in another mission is not ours.
        assert_eq!(
            cost_for(&entries, "other-1", "planner"),
            MicroUsd::from_micros(700)
        );
        // A role that made no call spent nothing.
        assert_eq!(cost_for(&entries, "live-1", "absent"), MicroUsd::ZERO);
        assert_eq!(
            spent_for(&path, "live-1", "reviewer"),
            MicroUsd::from_micros(40)
        );
        assert_eq!(
            spent_for(&path, "live-1", "absent"),
            MicroUsd::ZERO,
            "unknown role is zero"
        );
        assert_eq!(
            spent_for(&dir.path().join("absent.jsonl"), "live-1", "planner"),
            MicroUsd::ZERO,
            "a missing ledger spends nothing"
        );
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
    fn a_torn_line_costs_only_itself_in_the_lenient_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let good = serde_json::to_string(&entry("T-1", Outcome::Completed)).unwrap();
        std::fs::write(
            &path,
            format!(
                "{good}
{{\"mission\":\"tor
{good}
"
            ),
        )
        .unwrap();
        let (entries, skipped) = read_entries_lenient(&path);
        assert_eq!(entries.len(), 2, "both whole rows still count");
        assert_eq!(skipped, 1);
        assert_eq!(total_cost(&entries), MicroUsd::from_micros(217_200 * 2));
        assert_eq!(
            read_entries_lenient(&dir.path().join("absent.jsonl")),
            (Vec::new(), 0)
        );
    }

    /// A real tear: a crash or a full disk cuts a rejected row's free-text
    /// reason inside a multi-byte character, with no newline after it. The
    /// file is then not UTF-8, which made the lenient reader see no ledger at
    /// all, and the next row went onto the torn line and was lost with it.
    #[test]
    fn a_tear_inside_a_character_costs_only_the_torn_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        append_entry(&path, &entry("T-1", Outcome::Completed)).unwrap();
        let rejected = serde_json::to_string(&entry(
            "T-2",
            Outcome::Rejected {
                reason: "quality gate failed: “tests” red".into(),
            },
        ))
        .unwrap();
        let cut = rejected.find('“').unwrap() + 1;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&rejected.as_bytes()[..cut]).unwrap();
        drop(file);
        append_entry(&path, &entry("T-3", Outcome::Completed)).unwrap();

        let (entries, skipped) = read_entries_lenient(&path);
        let tasks: Vec<&str> = entries.iter().map(|e| e.task.as_str()).collect();
        assert_eq!(tasks, ["T-1", "T-3"], "the row after the tear still counts");
        assert_eq!(skipped, 1);
        assert_eq!(total_cost(&entries), MicroUsd::from_micros(217_200 * 2));
    }

    /// harness-mission reads each role's spend back with [`spent_for`], and
    /// the supervisor each merged task's cost with
    /// [`crate::lifecycle::task_cost`]. Both read the ledger strictly, so one
    /// torn line, inside a character or not, made every later readback $0.
    #[test]
    fn a_tear_costs_only_the_torn_row_in_the_spend_readbacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        append_entry(&path, &charged("live-1", "planner", 120)).unwrap();
        let rejected = serde_json::to_string(&entry(
            "T-2",
            Outcome::Rejected {
                reason: "quality gate failed: “tests” red".into(),
            },
        ))
        .unwrap();
        let cut = rejected.find('“').unwrap() + 1;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&rejected.as_bytes()[..cut]).unwrap();
        drop(file);
        // The next row lands on the torn line.
        append_entry(&path, &charged("live-1", "planner", 80)).unwrap();
        append_entry(&path, &charged("live-1", "planner", 40)).unwrap();
        assert!(std::str::from_utf8(&std::fs::read(&path).unwrap()).is_err());

        assert_eq!(
            spent_for(&path, "live-1", "planner"),
            MicroUsd::from_micros(240)
        );
        assert_eq!(
            crate::lifecycle::task_cost(&path, "live-1", "planner"),
            MicroUsd::from_micros(240)
        );
    }

    /// An ASCII tear has no newline after it either, so the next append is
    /// glued onto the fragment's line. That row is still read, by every
    /// lenient reader, whatever row type it reads into.
    #[test]
    fn a_row_appended_onto_a_torn_line_still_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let torn = r#"{"mission":"m","task":"torn","usage":{"input":1,"cache_hit":0"#;
        std::fs::write(&path, torn).unwrap();
        append_entry(&path, &entry("T-1", Outcome::Completed)).unwrap();
        append_entry(&path, &entry("T-2", Outcome::Completed)).unwrap();

        let (entries, skipped) = read_entries_lenient(&path);
        let tasks: Vec<&str> = entries.iter().map(|e| e.task.as_str()).collect();
        assert_eq!(tasks, ["T-1", "T-2"]);
        assert_eq!(skipped, 1, "only the fragment is lost");
        let (values, skipped) = read_rows_lenient::<serde_json::Value>(&path);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["task"], "T-1");
        assert_eq!(skipped, 1);
        // A line that is not a torn row yields nothing from its inner objects.
        std::fs::write(&path, "{\"outcome\":{\"unknown\":{\"x\":1}}}\n").unwrap();
        assert_eq!(read_entries_lenient(&path), (Vec::new(), 1));
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        assert!(read_entries(&path).is_err());
    }
}
