//! Time-Travel Debugger & Immutable Event Log (Swarm OS Bullet 7)
//!
//! Wraps the HMAC-authenticated `audit_chain` to provide a cryptographically
//! immutable history of all cell interactions, intent routing, and state changes.
//! Enables time-travel debugging and forensic analysis without the possibility
//! of post-facto tampering.

use crate::susi_abi::wire::WireFrame;
use std::path::{Path, PathBuf};

pub struct TimeTravelDebugger {
    audit_file: PathBuf,
}

impl TimeTravelDebugger {
    /// Initializes the time-travel debugger with the workspace's audit log.
    pub fn new(workspace: &Path) -> Self {
        Self {
            audit_file: workspace.join("swarm_events.audit.log"),
        }
    }

    /// Logs a raw wire frame interaction (IPC entry/exit point).
    pub fn log_wire_frame(&self, direction: &str, cell_id: &str, frame: &WireFrame) {
        let details = format!(
            "direction={} cell={} type={:?} qos={:?} payload_len={}",
            direction,
            cell_id,
            frame.msg_type,
            frame.qos,
            frame.payload.len()
        );

        let _ = crate::susi_sandbox::audit_chain::append_signed_entry(
            &self.audit_file,
            "INFO",
            "WIRE_FRAME",
            &details,
            std::process::id(),
        );
    }

    /// Logs a Swarm OS state transition or scheduler decision.
    pub fn log_state_change(&self, component: &str, details: &str) {
        let _ = crate::susi_sandbox::audit_chain::append_signed_entry(
            &self.audit_file,
            "INFO",
            component,
            details,
            std::process::id(),
        );
    }

    /// Verifies the cryptographic integrity of the entire event log history.
    pub fn verify_history(&self) -> Result<usize, String> {
        crate::susi_sandbox::audit_chain::verify_chain(&self.audit_file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_abi::wire::{MessageType, WireFrame};
    use std::env;
    use std::fs;

    #[test]
    fn test_time_travel_logger() {
        let temp_dir = env::temp_dir().join(format!("susi_tt_test_{}", std::process::id()));
        let logger = TimeTravelDebugger::new(&temp_dir);

        // Log some events
        let frame = WireFrame::new(MessageType::Heartbeat, b"ping".to_vec());
        logger.log_wire_frame("RX", "cell-1", &frame);
        logger.log_state_change("SCHEDULER", "Assigned task-1 to cell-1");

        // Verify cryptographic chain
        let entries = logger.verify_history().expect("Chain should be valid");
        assert_eq!(entries, 2);

        let _ = fs::remove_dir_all(temp_dir);
    }
}
