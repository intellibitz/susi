//! The control surface every cockpit speaks.
//!
//! A cockpit — the harness panel, a browser, an agent shelling out to the CLI —
//! sends a [`ControlCommand`] and receives a [`ControlAck`]. The command is the
//! same JSON on every transport, which is what lets one panel serve DSH, a
//! browser and an MCP client without a second implementation.

use crate::money::MicroUsd;
use serde::{Deserialize, Serialize};
use susi_error::{EaiError, EaiResult};

/// What the operator asked a mission to build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionSpec {
    /// Stable mission id, unique within a supervisor.
    pub id: String,
    /// Human-readable title, the only thing the operator reads at start.
    pub title: String,
    /// Roadmap vector this mission delivers, when it delivers one.
    pub roadmap: Option<String>,
}

impl MissionSpec {
    /// Reject an id that cannot be a file name or a URL segment, and an empty
    /// title — a mission without either cannot be reported on.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` naming the offending field.
    pub fn validate(&self) -> EaiResult<()> {
        if self.id.trim().is_empty() {
            return Err(EaiError::config("mission id is required"));
        }
        if self.id.len() > 64 {
            return Err(EaiError::config("mission id must be at most 64 characters"));
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(EaiError::config(
                "mission id may contain only ASCII letters, digits, '-', '_' and '.'",
            ));
        }
        if self.title.trim().is_empty() {
            return Err(EaiError::config("mission title is required"));
        }
        Ok(())
    }
}

/// Where a mission stands, in the operator's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    /// Accepted but not dispatched.
    Planned,
    /// Executing.
    Running,
    /// Stopped by the operator or by credit exhaustion.
    Paused,
    /// The capability ladder was exhausted; a decision is owed.
    Stuck,
    /// Acceptance passed.
    Done,
    /// Cancelled by the operator.
    Aborted,
}

/// A command from a cockpit to the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Start a mission.
    StartMission {
        /// What to build.
        mission: MissionSpec,
    },
    /// Pause a running mission at the next safe point.
    Pause {
        /// Mission to pause.
        mission_id: String,
    },
    /// Resume a paused mission.
    Resume {
        /// Mission to resume.
        mission_id: String,
    },
    /// Abandon a mission; committed work is kept, uncommitted work is not.
    Abort {
        /// Mission to abort.
        mission_id: String,
    },
    /// Change a mission's queue priority (0 is highest).
    Redirect {
        /// Mission to redirect.
        mission_id: String,
        /// New priority, 0–9.
        priority: u8,
    },
    /// Record a provider top-up so the ledger's projection stops warning.
    TopUp {
        /// Provider id.
        provider: String,
        /// Amount added.
        amount: MicroUsd,
    },
    /// Answer a decision the supervisor is blocked on.
    Decide {
        /// Decision id from the event feed.
        decision_id: String,
        /// The operator's choice, in the supervisor's own option vocabulary.
        choice: String,
    },
}

impl ControlCommand {
    /// The mission this command targets, when it targets one.
    #[must_use]
    pub fn mission_id(&self) -> Option<&str> {
        match self {
            Self::StartMission { mission } => Some(mission.id.as_str()),
            Self::Pause { mission_id }
            | Self::Resume { mission_id }
            | Self::Abort { mission_id }
            | Self::Redirect { mission_id, .. } => Some(mission_id.as_str()),
            Self::TopUp { .. } | Self::Decide { .. } => None,
        }
    }

    /// Reject a command that cannot be acted on.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` naming the offending field.
    pub fn validate(&self) -> EaiResult<()> {
        match self {
            Self::StartMission { mission } => mission.validate(),
            Self::Pause { mission_id }
            | Self::Resume { mission_id }
            | Self::Abort { mission_id } => require(mission_id, "mission_id"),
            Self::Redirect {
                mission_id,
                priority,
            } => {
                require(mission_id, "mission_id")?;
                if *priority > 9 {
                    return Err(EaiError::config("priority must be 0-9"));
                }
                Ok(())
            }
            Self::TopUp { provider, amount } => {
                require(provider, "provider")?;
                if amount.is_zero() {
                    return Err(EaiError::config("top-up amount must be positive"));
                }
                Ok(())
            }
            Self::Decide {
                decision_id,
                choice,
            } => {
                require(decision_id, "decision_id")?;
                require(choice, "choice")
            }
        }
    }
}

/// The supervisor's answer to a [`ControlCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAck {
    /// Whether the command was accepted.
    pub accepted: bool,
    /// Why, in the operator's language.
    pub message: String,
}

impl ControlAck {
    /// The command was accepted.
    #[must_use]
    pub fn accepted(message: impl Into<String>) -> Self {
        Self {
            accepted: true,
            message: message.into(),
        }
    }

    /// The command was refused; nothing changed.
    #[must_use]
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            accepted: false,
            message: message.into(),
        }
    }
}

fn require(value: &str, field: &str) -> EaiResult<()> {
    if value.trim().is_empty() {
        return Err(EaiError::config(format!("{field} is required")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> MissionSpec {
        MissionSpec {
            id: id.into(),
            title: "Harden agent identity".into(),
            roadmap: Some("VC-202-025".into()),
        }
    }

    #[test]
    fn commands_round_trip_with_a_tag() {
        let command = ControlCommand::StartMission {
            mission: spec("m-1"),
        };
        let json = serde_json::to_string(&command).unwrap();
        assert!(json.contains("\"command\":\"start_mission\""));
        assert_eq!(
            serde_json::from_str::<ControlCommand>(&json).unwrap(),
            command
        );

        let top_up = ControlCommand::TopUp {
            provider: "deepseek".into(),
            amount: MicroUsd::from_micros(50_000_000),
        };
        let json = serde_json::to_string(&top_up).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlCommand>(&json).unwrap(),
            top_up
        );
    }

    #[test]
    fn mission_scope_is_reported() {
        assert_eq!(spec("m-1").id, "m-1");
        let pause = ControlCommand::Pause {
            mission_id: "m-1".into(),
        };
        assert_eq!(pause.mission_id(), Some("m-1"));
        let top_up = ControlCommand::TopUp {
            provider: "gemini".into(),
            amount: MicroUsd::from_micros(1),
        };
        assert_eq!(top_up.mission_id(), None);
    }

    #[test]
    fn bad_commands_are_refused() {
        assert!(spec("").validate().is_err());
        assert!(spec("with space").validate().is_err());
        assert!(spec(&"x".repeat(65)).validate().is_err());
        assert!(ControlCommand::Redirect {
            mission_id: "m".into(),
            priority: 10,
        }
        .validate()
        .is_err());
        assert!(ControlCommand::TopUp {
            provider: "deepseek".into(),
            amount: MicroUsd::ZERO,
        }
        .validate()
        .is_err());
        assert!(ControlCommand::Decide {
            decision_id: "d-1".into(),
            choice: "".into(),
        }
        .validate()
        .is_err());
    }

    #[test]
    fn a_valid_start_is_accepted() {
        assert!(spec("vc-202-025").validate().is_ok());
        assert!(ControlCommand::StartMission {
            mission: spec("m_1.2")
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn acks_say_what_happened() {
        assert!(ControlAck::accepted("queued").accepted);
        assert!(!ControlAck::refused("unknown mission").accepted);
    }
}
