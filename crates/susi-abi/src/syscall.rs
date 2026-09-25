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
