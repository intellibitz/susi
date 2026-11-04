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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_status_serializes_to_exact_snake_case_wire_tags() {
        let cases = [
            (ReceiptStatus::Success, "\"success\""),
            (ReceiptStatus::Failure, "\"failure\""),
            (ReceiptStatus::Denied, "\"denied\""),
            (ReceiptStatus::Timeout, "\"timeout\""),
        ];
        for (status, expected_json) in cases {
            let encoded = serde_json::to_string(&status).expect("serialize status");
            assert_eq!(encoded, expected_json);
            let decoded: ReceiptStatus =
                serde_json::from_str(&encoded).expect("deserialize status");
            assert_eq!(decoded, status);
        }
    }

    #[test]
    fn tool_receipt_grounded_claim_and_session_summary_round_trip_serde() {
        let receipt_with_summary = ToolReceipt {
            receipt_id: "rcpt-001".to_string(),
            tool_name: "fs.read".to_string(),
            args_hash: "sha256:aaa111".to_string(),
            output_hash: "sha256:bbb222".to_string(),
            status: ReceiptStatus::Success,
            duration_us: 1450,
            observed_at: 1_700_000_100,
            summary: Some("Read 42 lines from Cargo.toml".to_string()),
        };
        let encoded_receipt =
            serde_json::to_string(&receipt_with_summary).expect("serialize receipt");
        let decoded_receipt: ToolReceipt =
            serde_json::from_str(&encoded_receipt).expect("deserialize receipt");
        assert_eq!(decoded_receipt, receipt_with_summary);

        let receipt_without_summary = ToolReceipt {
            summary: None,
            status: ReceiptStatus::Denied,
            ..receipt_with_summary
        };
        let encoded_none =
            serde_json::to_string(&receipt_without_summary).expect("serialize none summary");
        let decoded_none: ToolReceipt =
            serde_json::from_str(&encoded_none).expect("deserialize none summary");
        assert_eq!(decoded_none, receipt_without_summary);
        assert!(decoded_none.summary.is_none());

        let claim = GroundedClaim {
            claim_id: "claim-1104".to_string(),
            proposition: "All unit tests pass in susi-abi".to_string(),
            confidence: 0.98,
            receipt_citations: vec!["rcpt-001".to_string(), "rcpt-002".to_string()],
            verified: true,
        };
        let encoded_claim = serde_json::to_string(&claim).expect("serialize claim");
        let decoded_claim: GroundedClaim =
            serde_json::from_str(&encoded_claim).expect("deserialize claim");
        assert_eq!(decoded_claim, claim);

        let session = EvidenceSessionSummary {
            session_id: "sess-99".to_string(),
            workspace_root: "/workspace/susi".to_string(),
            total_receipts: 12,
            verified_claims: 5,
            created_at: 1_700_000_200,
        };
        let encoded_session = serde_json::to_string(&session).expect("serialize session");
        let decoded_session: EvidenceSessionSummary =
            serde_json::from_str(&encoded_session).expect("deserialize session");
        assert_eq!(decoded_session, session);
    }
}
