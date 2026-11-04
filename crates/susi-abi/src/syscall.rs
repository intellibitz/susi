//! SUSI Swarm OS AI Syscall Definitions.
//!
//! Language-agnostic syscall interface for autonomous AI agents and runtime drivers.

use serde::{Deserialize, Serialize};

/// Standard AI Syscall Operations supported by the SUSI Microkernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
#[serde(rename_all = "snake_case")]
pub enum SyscallOp {
    /// 0x01: Route prompt / token request to neural driver (local CUDA or cloud).
    Infer = 0x01,
    /// 0x02: Execute capability/tool under MAC governance; emits ToolReceipt.
    ToolCall = 0x02,
    /// 0x03: Allocate or pin attention KV cache tokens in virtual memory.
    ContextAlloc = 0x03,
    /// 0x04: Deposit a pheromone/intent/observation into the shared blackboard.
    BlackboardPost = 0x04,
    /// 0x05: Query blackboard entries matching topic or capability bloom filter.
    BlackboardRead = 0x05,
    /// 0x06: Cast signed vote into the quorum consensus ledger.
    ConsensusVote = 0x06,
    /// 0x07: Hot-mount sandboxed Wasm reflex without daemon restart.
    ReflexMount = 0x07,
    /// 0x08: Seal claim or observation into immutable HMAC audit ledger.
    AuditSeal = 0x08,
    /// 0x09: Periodic cell liveness / heartbeat ping.
    Heartbeat = 0x09,
    /// 0x0A: Inspect hardware telemetry (GPU/CPU saturation, memory).
    TelemetryGet = 0x0A,
    /// 0x0B: Clone cell memory state and parallelize (Bullet 83).
    Fork = 0x0B,
    /// 0x0C: Verify Ed25519 signature of an action (Bullet 56).
    IdentityVerify = 0x0C,
    /// 0x0D: Cooperative multitasking yield (Bullet 10).
    Yield = 0x0D,
}

impl SyscallOp {
    /// Returns the integer opcode for binary IPC framing.
    #[must_use]
    pub const fn opcode(self) -> u32 {
        self as u32
    }

    /// Reconstructs a syscall opcode from a raw integer.
    #[must_use]
    pub const fn from_opcode(code: u32) -> Option<Self> {
        match code {
            0x01 => Some(Self::Infer),
            0x02 => Some(Self::ToolCall),
            0x03 => Some(Self::ContextAlloc),
            0x04 => Some(Self::BlackboardPost),
            0x05 => Some(Self::BlackboardRead),
            0x06 => Some(Self::ConsensusVote),
            0x07 => Some(Self::ReflexMount),
            0x08 => Some(Self::AuditSeal),
            0x09 => Some(Self::Heartbeat),
            0x0A => Some(Self::TelemetryGet),
            0x0B => Some(Self::Fork),
            0x0C => Some(Self::IdentityVerify),
            0x0D => Some(Self::Yield),
            _ => None,
        }
    }
}

/// Status of a completed or rejected syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyscallStatus {
    /// Syscall executed successfully.
    Success,
    /// Syscall was denied by MAC policy, sandbox boundary, or authorization.
    Denied,
    /// Target capability, topic, or model resource not found.
    NotFound,
    /// Syscall timed out before completing.
    Timeout,
    /// Internal runtime or driver error.
    Error,
}

/// An incoming AI Syscall request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyscallRequest {
    /// Monotonic or UUID correlation id for the request.
    pub id: String,
    /// Identity of the calling agent, swarm cell, or process.
    pub caller_id: String,
    /// Syscall operation to perform.
    pub op: SyscallOp,
    /// Optional Bearer or capability token.
    pub token: Option<String>,
    /// Target workspace path for context-jailed execution.
    pub workspace: Option<String>,
    /// JSON payload arguments.
    pub payload: serde_json::Value,
    /// Timestamp in Unix seconds.
    pub timestamp: u64,
}

/// The response returned from an AI Syscall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyscallResponse {
    /// Correlation id matching the request.
    pub id: String,
    /// Execution status.
    pub status: SyscallStatus,
    /// Execution payload or result data.
    pub data: serde_json::Value,
    /// Optional verifiable receipt for grounded actions.
    pub receipt: Option<super::evidence::ToolReceipt>,
    /// Execution latency in microseconds.
    pub latency_us: u64,
    /// Optional human-readable error or diagnosis message.
    pub message: Option<String>,
}

/// Environment variable carrying the bearer token a Swarm Cell requires on
/// every syscall. Spawners set it from the host API token.
pub const CELL_TOKEN_ENV: &str = "SUSI_CELL_TOKEN";

/// Constant-time check that a request presented the expected cell token.
///
/// An empty `expected` never authorizes (fail closed), and a missing token
/// never matches.
#[must_use]
pub fn token_matches(presented: Option<&str>, expected: &str) -> bool {
    let Some(presented) = presented else {
        return false;
    };
    let (a, b) = (presented.as_bytes(), expected.as_bytes());
    if b.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod token_tests {
    use super::{token_matches, SyscallOp};

    #[test]
    fn token_matching_is_exact_and_fails_closed() {
        assert!(token_matches(Some("s3cret"), "s3cret"));
        assert!(!token_matches(Some("s3creT"), "s3cret"));
        assert!(!token_matches(Some("s3cret-extra"), "s3cret"));
        assert!(!token_matches(None, "s3cret"));
        assert!(!token_matches(Some(""), ""));
        assert!(!token_matches(Some("anything"), ""));
    }

    #[test]
    fn all_thirteen_syscall_opcodes_biject_and_round_trip_serde() {
        let table = [
            (0x01, SyscallOp::Infer, "\"infer\""),
            (0x02, SyscallOp::ToolCall, "\"tool_call\""),
            (0x03, SyscallOp::ContextAlloc, "\"context_alloc\""),
            (0x04, SyscallOp::BlackboardPost, "\"blackboard_post\""),
            (0x05, SyscallOp::BlackboardRead, "\"blackboard_read\""),
            (0x06, SyscallOp::ConsensusVote, "\"consensus_vote\""),
            (0x07, SyscallOp::ReflexMount, "\"reflex_mount\""),
            (0x08, SyscallOp::AuditSeal, "\"audit_seal\""),
            (0x09, SyscallOp::Heartbeat, "\"heartbeat\""),
            (0x0A, SyscallOp::TelemetryGet, "\"telemetry_get\""),
            (0x0B, SyscallOp::Fork, "\"fork\""),
            (0x0C, SyscallOp::IdentityVerify, "\"identity_verify\""),
            (0x0D, SyscallOp::Yield, "\"yield\""),
        ];
        assert_eq!(table.len(), 13);
        for (expected_code, op, expected_json) in table {
            assert_eq!(op.opcode(), expected_code);
            assert_eq!(SyscallOp::from_opcode(expected_code), Some(op));
            let encoded = serde_json::to_string(&op).expect("serialize SyscallOp");
            assert_eq!(encoded, expected_json);
            let decoded: SyscallOp = serde_json::from_str(&encoded).expect("deserialize SyscallOp");
            assert_eq!(decoded, op);
        }
        for invalid in [0x00, 0x0E, 0xFF, u32::MAX] {
            assert_eq!(SyscallOp::from_opcode(invalid), None);
        }
    }

    #[test]
    fn syscall_status_round_trips_all_five_snake_case_variants() {
        use super::SyscallStatus;
        let cases = [
            (SyscallStatus::Success, "\"success\""),
            (SyscallStatus::Denied, "\"denied\""),
            (SyscallStatus::NotFound, "\"not_found\""),
            (SyscallStatus::Timeout, "\"timeout\""),
            (SyscallStatus::Error, "\"error\""),
        ];
        for (status, expected_json) in cases {
            let encoded = serde_json::to_string(&status).expect("serialize SyscallStatus");
            assert_eq!(encoded, expected_json);
            let decoded: SyscallStatus =
                serde_json::from_str(&encoded).expect("deserialize SyscallStatus");
            assert_eq!(decoded, status);
        }
    }

    #[test]
    fn syscall_request_and_response_round_trip_serde_with_and_without_optional_fields() {
        use super::{SyscallOp, SyscallRequest, SyscallResponse, SyscallStatus};
        use crate::evidence::{ReceiptStatus, ToolReceipt};

        let req_minimal = SyscallRequest {
            id: "req-min-01".to_string(),
            caller_id: "agent-alpha".to_string(),
            op: SyscallOp::Heartbeat,
            token: None,
            workspace: None,
            payload: serde_json::Value::Null,
            timestamp: 1_700_000_000,
        };
        let req_min_json = serde_json::to_string(&req_minimal).expect("serialize minimal request");
        let req_min_back: SyscallRequest =
            serde_json::from_str(&req_min_json).expect("deserialize minimal request");
        assert_eq!(req_min_back, req_minimal);

        let req_full = SyscallRequest {
            id: "req-full-02".to_string(),
            caller_id: "cell-gawd".to_string(),
            op: SyscallOp::ToolCall,
            token: Some("bearer-tok-xyz".to_string()),
            workspace: Some("/workspace/project".to_string()),
            payload: serde_json::json!({"cmd": "cargo test", "timeout_ms": 5000}),
            timestamp: 1_700_000_123,
        };
        let req_full_json = serde_json::to_string(&req_full).expect("serialize full request");
        let req_full_back: SyscallRequest =
            serde_json::from_str(&req_full_json).expect("deserialize full request");
        assert_eq!(req_full_back, req_full);

        let resp_minimal = SyscallResponse {
            id: "req-min-01".to_string(),
            status: SyscallStatus::Success,
            data: serde_json::json!({"alive": true}),
            receipt: None,
            latency_us: 42,
            message: None,
        };
        let resp_min_json =
            serde_json::to_string(&resp_minimal).expect("serialize minimal response");
        let resp_min_back: SyscallResponse =
            serde_json::from_str(&resp_min_json).expect("deserialize minimal response");
        assert_eq!(resp_min_back, resp_minimal);

        let receipt = ToolReceipt {
            receipt_id: "rcpt-syscall-99".to_string(),
            tool_name: "shell_exec".to_string(),
            args_hash: "sha256:1111".to_string(),
            output_hash: "sha256:2222".to_string(),
            status: ReceiptStatus::Success,
            duration_us: 980,
            observed_at: 1_700_000_124,
            summary: Some("0 errors".to_string()),
        };
        let resp_full = SyscallResponse {
            id: "req-full-02".to_string(),
            status: SyscallStatus::Error,
            data: serde_json::json!({"exit_code": 1}),
            receipt: Some(receipt),
            latency_us: 1024,
            message: Some("command exited with status 1".to_string()),
        };
        let resp_full_json = serde_json::to_string(&resp_full).expect("serialize full response");
        let resp_full_back: SyscallResponse =
            serde_json::from_str(&resp_full_json).expect("deserialize full response");
        assert_eq!(resp_full_back, resp_full);
    }
}
