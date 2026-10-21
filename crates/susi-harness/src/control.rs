//! Operator control from the cockpit.
//!
//! The cockpit is a control surface, not a second source of truth. A command is
//! validated, appended to a control log, and answered. The supervisor reads the
//! log at the next task boundary and applies it; the log is the durable record
//! of what the operator asked, even if the mission process is not running.
//!
//! Pause and abort are cooperative: the current turn finishes, then no new task
//! starts. That is deliberate. Killing a turn mid-call would still pay for the
//! tokens it consumed and would leave a worktree no one knows the state of.

use crate::api::{ControlAck, ControlCommand};
use crate::money::MicroUsd;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// One command and the answer the cockpit gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRecord {
    /// Milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// What the operator asked.
    pub command: ControlCommand,
    /// Whether it was accepted.
    pub accepted: bool,
    /// Why, in the operator's language.
    pub message: String,
}

impl ControlRecord {
    /// Validate `command` and build the record the cockpit stores.
    #[must_use]
    pub fn answer(command: ControlCommand) -> Self {
        let (accepted, message) = match command.validate() {
            Ok(()) => (true, "accepted".to_string()),
            Err(error) => (false, error.to_string()),
        };
        Self {
            at_unix_ms: now_ms(),
            command,
            accepted,
            message,
        }
    }
}

/// Append one record as a JSON line.
///
/// # Errors
///
/// Returns `EaiError::io` when the record cannot be encoded or opened.
pub fn append_record(path: &Path, record: &ControlRecord) -> EaiResult<()> {
    let line = serde_json::to_string(record)
        .map_err(|error| EaiError::io(format!("encode control record: {error}")))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| EaiError::io(format!("open control log {}: {error}", path.display())))?;
    writeln!(file, "{line}")
        .map_err(|error| EaiError::io(format!("append control log {}: {error}", path.display())))
}

/// Read every record in append order. A missing log is empty, not an error.
#[must_use]
pub fn read_records(path: &Path) -> Vec<ControlRecord> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The operator's standing intent after applying the accepted commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlState {
    /// True when the last accepted pause/resume left the mission paused.
    pub paused: bool,
    /// True once the operator aborted the mission.
    pub aborted: bool,
    /// Highest-priority mission redirect (0 is highest).
    pub priority: Option<u8>,
    /// Provider top-ups, cumulative in micro-USD.
    pub topups: BTreeMap<String, MicroUsd>,
    /// Operator answers to decisions, by decision id.
    pub decisions: BTreeMap<String, String>,
}

impl ControlState {
    /// Derive the standing intent from accepted records, in order.
    #[must_use]
    pub fn from_records(records: &[ControlRecord]) -> Self {
        let mut state = Self::default();
        for record in records.iter().filter(|record| record.accepted) {
            state.apply(&record.command);
        }
        state
    }

    /// Read the log and derive the standings.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        Self::from_records(&read_records(path))
    }

    /// True when no new task may start.
    #[must_use]
    pub const fn blocks_new_work(&self) -> bool {
        self.paused || self.aborted
    }

    fn apply(&mut self, command: &ControlCommand) {
        match command {
            ControlCommand::Pause { .. } => self.paused = true,
            ControlCommand::Resume { .. } => self.paused = false,
            ControlCommand::Abort { .. } => {
                self.aborted = true;
                self.paused = false;
            }
            ControlCommand::Redirect { priority, .. } => self.priority = Some(*priority),
            ControlCommand::TopUp { provider, amount } => {
                let slot = self.topups.entry(provider.clone()).or_default();
                *slot = slot.saturating_add(*amount);
            }
            ControlCommand::Decide {
                decision_id,
                choice,
            } => {
                self.decisions.insert(decision_id.clone(), choice.clone());
            }
            ControlCommand::StartMission { .. } => {}
        }
    }
}

/// Validate, store and answer a command. A refused command changes nothing.
///
/// # Errors
///
/// Returns `EaiError::io` when an accepted record cannot be written. A refused
/// command is still recorded, so the operator can see why it did nothing.
pub fn submit(path: &Path, command: ControlCommand) -> EaiResult<ControlAck> {
    let record = ControlRecord::answer(command);
    append_record(path, &record)?;
    Ok(if record.accepted {
        ControlAck::accepted(record.message)
    } else {
        ControlAck::refused(record.message)
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(command: ControlCommand) -> ControlRecord {
        ControlRecord {
            at_unix_ms: 0,
            command,
            accepted: true,
            message: "accepted".to_string(),
        }
    }

    #[test]
    fn pause_and_resume_are_ordered() {
        let records = vec![
            record(ControlCommand::Pause {
                mission_id: "m-1".into(),
            }),
            record(ControlCommand::Resume {
                mission_id: "m-1".into(),
            }),
        ];
        let state = ControlState::from_records(&records);
        assert!(!state.paused);
        assert!(!state.blocks_new_work());
    }

    #[test]
    fn abort_is_sticky_and_stops_new_work() {
        let state = ControlState::from_records(&[record(ControlCommand::Abort {
            mission_id: "m-1".into(),
        })]);
        assert!(state.aborted);
        assert!(state.blocks_new_work());
    }

    #[test]
    fn topups_accumulate_per_provider() {
        let state = ControlState::from_records(&[
            record(ControlCommand::TopUp {
                provider: "deepseek".into(),
                amount: MicroUsd::from_micros(1_000_000),
            }),
            record(ControlCommand::TopUp {
                provider: "deepseek".into(),
                amount: MicroUsd::from_micros(2_000_000),
            }),
        ]);
        assert_eq!(
            state.topups.get("deepseek"),
            Some(&MicroUsd::from_micros(3_000_000))
        );
    }

    #[test]
    fn a_refused_command_changes_nothing() {
        let refused = ControlRecord {
            at_unix_ms: 0,
            command: ControlCommand::Pause {
                mission_id: String::new(),
            },
            accepted: false,
            message: "mission_id is required".into(),
        };
        assert!(!ControlState::from_records(&[refused]).paused);
    }

    #[test]
    fn a_missing_log_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_records(&dir.path().join("absent.jsonl")).is_empty());
    }

    #[test]
    fn submit_refuses_an_invalid_command_but_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.jsonl");
        let ack = submit(
            &path,
            ControlCommand::Pause {
                mission_id: String::new(),
            },
        )
        .unwrap();
        assert!(!ack.accepted);
        let records = read_records(&path);
        assert_eq!(records.len(), 1);
        assert!(!records[0].accepted);
    }
}
