//! SUSI Swarm OS Evidence and Receipt DTOs.
//!
//! Immutable data structures for grounding agent outputs in verifiable reality.

use serde::{Deserialize, Serialize};

/// Execution status of a tool or capability invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// Tool completed successfully with verifiable output.
    Success,
    /// Tool execution returned an error or nonzero exit code.
    Failure,
    /// Tool execution was blocked by Mandatory Access Control (MAC) or policy.
    Denied,
    /// Tool execution exceeded deadline without completion.
    Timeout,
}

/// Cryptographically verifiable receipt representing a completed tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReceipt {
    /// Unique identifier for this receipt.
    pub receipt_id: String,
    /// Name of the capability or tool invoked.
    pub tool_name: String,
    /// SHA-256 hash or digest of the arguments provided.
    pub args_hash: String,
    /// SHA-256 hash or digest of the raw output received.
    pub output_hash: String,
    /// Execution status.
    pub status: ReceiptStatus,
    /// Execution duration in microseconds.
    pub duration_us: u64,
    /// Unix timestamp in seconds when the observation was recorded.
    pub observed_at: u64,
    /// Optional truncated summary of the observation for human inspection.
    pub summary: Option<String>,
}

/// An epistemic claim made by an agent, grounded by supporting receipts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroundedClaim {
    /// Unique identifier for the claim.
    pub claim_id: String,
    /// The subject or proposition being asserted.
    pub proposition: String,
    /// Confidence score between 0.0 and 1.0.
    pub confidence: f32,
    /// List of receipt IDs grounding this claim in workspace reality.
    pub receipt_citations: Vec<String>,
    /// Epistemic verification status.
    pub verified: bool,
}

/// A complete evidence session snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSessionSummary {
    /// Session identifier.
    pub session_id: String,
    /// Workspace root path where execution occurred.
    pub workspace_root: String,
    /// Total tool receipts accumulated.
    pub total_receipts: usize,
    /// Total claims verified against ground truth.
    pub verified_claims: usize,
    /// Session creation timestamp.
    pub created_at: u64,
}
