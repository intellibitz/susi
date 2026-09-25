//! Bridge from susi-core's internal receipt bookkeeping to the shared
//! `susi-abi` wire vocabulary.
//!
//! Kept out of `capture.rs` on purpose: that file is vendored byte-identical
//! into 9 zero-dependency consumer crates (`scripts/check-vendored-sync.sh`),
//! and susi-core is currently the only crate with a real Cargo edge to
//! `susi-abi`. This file stays outside the vendored `src/susi_core/` tree so
//! that edge never leaks into the vendored copies.

use crate::capture::ToolReceipt;
use crate::evidence::{EvidenceAssessment, EvidenceRecord, EvidenceSource};
use crate::plane_bus::PlaneBus;
use std::path::Path;
use std::time::Instant;
use susi_abi::evidence::{GroundedClaim, ReceiptStatus};
use susi_abi::syscall::{SyscallOp, SyscallRequest, SyscallResponse, SyscallStatus};

/// Maps ABI syscall opcodes to susi-core's `plane_bus` topic-string dispatch,
/// where a genuine 1:1 correspondence exists today.
pub trait SyscallTopic {
    /// The `plane_bus` topic this syscall currently routes to, or `None`
    /// where no exact match exists. Only `Infer`/`ToolCall` map: every other
    /// opcode was checked against its most plausible-looking topic's actual
    /// handler and turned out either to bypass `plane_bus` entirely
    /// (`ContextAlloc`, `BlackboardPost`/`BlackboardRead` touch
    /// `HighDensityContextStore` directly; `ConsensusVote` is direct calls
    /// in `susi-gawd-swarm`'s `amas.rs`; `Heartbeat` is the daemon's UDP
    /// ping/pong, not a bus topic; `AuditSeal`'s real counterpart,
    /// `susi_sandbox::audit_chain::append_signed_entry`, is called directly
    /// from sandbox runtime code, never through the bus) or to only
    /// superficially resemble a topic (`ReflexMount` vs `GAWD_TRAIN_REFLEX`:
    /// that topic force-trains from patterns, not the hot-mount
    /// synthesize-then-retry flow behind `GAWD_CAPABILITY_GAP`, and even
    /// that only writes a `.wasm` file rather than mounting it; `TelemetryGet`
    /// is ambiguous between `GEMI_HARDWARE_PROFILE` and
    /// `GEMI_TELEMETRY_SAMPLE`, which report different things).
    fn plane_bus_topic(&self) -> Option<&'static str>;
}

impl SyscallTopic for SyscallOp {
    fn plane_bus_topic(&self) -> Option<&'static str> {
        match self {
            SyscallOp::Infer => Some(crate::plane_bus::topics::GEMI_INFER_GENERATE),
            SyscallOp::ToolCall => Some(crate::plane_bus::topics::TOOLS_EXECUTE),
            SyscallOp::ContextAlloc
            | SyscallOp::BlackboardPost
            | SyscallOp::BlackboardRead
            | SyscallOp::ConsensusVote
            | SyscallOp::ReflexMount
            | SyscallOp::AuditSeal
            | SyscallOp::Heartbeat
            | SyscallOp::TelemetryGet => None,
        }
    }
}

/// Executes a syscall request against `plane_bus`, for the opcodes
/// [`SyscallTopic::plane_bus_topic`] maps. An opcode with no mapping yet
/// returns `SyscallStatus::NotFound` rather than guessing a topic; a mapped
/// opcode whose plane never registered (composition root didn't wire that
/// feature crate in) surfaces as `SyscallStatus::Error` with `plane_bus`'s
/// own "no handler for topic" message.
pub fn dispatch_syscall(req: &SyscallRequest) -> SyscallResponse {
    let Some(topic) = req.op.plane_bus_topic() else {
        return SyscallResponse {
            id: req.id.clone(),
            status: SyscallStatus::NotFound,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us: 0,
            message: Some(format!("{:?} has no plane_bus mapping yet", req.op)),
        };
    };
    let start = Instant::now();
    let result = PlaneBus::global().request(topic, req.payload.clone());
    wrap_syscall_result(&req.id, result, start.elapsed())
}

/// Pure response-shaping split out of [`dispatch_syscall`] so it's unit
/// testable without touching `PlaneBus::global()` — that bus is a real,
/// machine-wide IPC rendezvous shared with any live `susi daemon-start`
/// process, not an in-process mock, so a test must never dispatch through it
/// with a real topic (either it silently calls into the live daemon, or
/// registering a fake handler to make the test deterministic would advertise
/// this test process as a real handler the live daemon could route
/// production requests into).
fn wrap_syscall_result(
    id: &str,
    result: Result<serde_json::Value, String>,
    elapsed: std::time::Duration,
) -> SyscallResponse {
    let latency_us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    match result {
        Ok(data) => SyscallResponse {
            id: id.to_string(),
            status: SyscallStatus::Success,
            data,
            receipt: None,
            latency_us,
            message: None,
        },
        Err(message) => SyscallResponse {
            id: id.to_string(),
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us,
            message: Some(message),
        },
    }
}

impl ToolReceipt {
    /// This receipt's outcome in the universal ABI vocabulary. `successful`
    /// only distinguishes pass/fail today; `Denied`/`Timeout` are reserved
    /// for when MAC denial and deadline expiry grow a distinct signal here.
    pub fn abi_status(&self) -> ReceiptStatus {
        if self.successful {
            ReceiptStatus::Success
        } else {
            ReceiptStatus::Failure
        }
    }
}

impl EvidenceRecord {
    /// This record in the universal ABI vocabulary. Re-runs [`Self::assess`]
    /// against `workspace` rather than trusting a caller-supplied verdict —
    /// `verified` is only ever as fresh as the reality check backing it.
    /// `receipt_citations` is non-empty only for [`EvidenceSource::ToolReceipt`]
    /// sources; the other five source kinds (file/command/MCP/system/agent
    /// observation) carry no live receipt id to cite.
    pub fn to_grounded_claim(&self, workspace: &Path) -> GroundedClaim {
        let receipt_citations = match &self.source {
            EvidenceSource::ToolReceipt { receipt_id, .. } => vec![receipt_id.clone()],
            EvidenceSource::File { .. }
            | EvidenceSource::Command { .. }
            | EvidenceSource::McpTool { .. }
            | EvidenceSource::System { .. }
            | EvidenceSource::AgentObservation { .. } => Vec::new(),
        };
        GroundedClaim {
            claim_id: self.signature.clone(),
            proposition: format!(
                "{} {} {}",
                self.claim.subject, self.claim.predicate, self.claim.value
            ),
            confidence: self.confidence,
            receipt_citations,
            verified: self.assess(workspace) == EvidenceAssessment::Verified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EvidenceSession;
    use crate::susi_error::EaiError;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "susi-abi-bridge-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn abi_status_reflects_success_and_failure() {
        let ws = Workspace::new();
        let session = EvidenceSession::new("mission", &ws.0, |s| s.to_string()).unwrap();
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call("ok_tool", &serde_json::json!(null), &ws.0, || {
            Ok("done".to_string())
        })
        .unwrap();
        EvidenceSession::capture_call("bad_tool", &serde_json::json!(null), &ws.0, || {
            Err(EaiError::process("boom"))
        })
        .unwrap_err();

        let receipts = session.receipts();
        let ok = receipts.iter().find(|r| r.tool == "ok_tool").unwrap();
        let bad = receipts.iter().find(|r| r.tool == "bad_tool").unwrap();
        assert_eq!(ok.abi_status(), ReceiptStatus::Success);
        assert_eq!(bad.abi_status(), ReceiptStatus::Failure);
    }

    #[test]
    fn grounded_claim_from_file_evidence_has_no_receipt_citation() {
        let ws = Workspace::new();
        std::fs::write(ws.0.join("source.txt"), "observed reality").unwrap();
        let record = EvidenceRecord::capture_file(
            "reader",
            &ws.0,
            Path::new("source.txt"),
            "exists",
            "true",
        )
        .unwrap();
        let claim = record.to_grounded_claim(&ws.0);
        assert_eq!(claim.claim_id, record.signature);
        assert_eq!(claim.proposition, "source.txt exists true");
        assert!(claim.verified);
        assert!(claim.receipt_citations.is_empty());

        let mut forged = record.clone();
        forged.claim.value = "false".into();
        forged.signature = String::new();
        let claim = forged.to_grounded_claim(&ws.0);
        assert!(!claim.verified, "unsigned/altered record must not verify");
    }

    #[test]
    fn grounded_claim_from_tool_receipt_carries_its_citation() {
        let ws = Workspace::new();
        let session = EvidenceSession::new("mission", &ws.0, |s| s.to_string()).unwrap();
        let _activation = EvidenceSession::activate(&session);
        EvidenceSession::capture_call(
            "weather",
            &serde_json::json!({"place": "Chennai"}),
            &ws.0,
            || Ok(r#"{"current":{"temperature_2m":24.8}}"#.to_string()),
        )
        .unwrap();
        let receipt = &session.receipts()[0];
        let record = EvidenceRecord::new(
            "search".into(),
            1.0,
            receipt.observed_at,
            crate::evidence::Claim {
                subject: "Chennai".into(),
                predicate: "temperature".into(),
                value: "24.8".into(),
            },
            EvidenceSource::ToolReceipt {
                receipt_id: receipt.id.clone(),
                tool: receipt.tool.clone(),
                output_hash: receipt.output_hash.clone(),
            },
            1.0,
        );
        let claim = record.to_grounded_claim(&ws.0);
        assert!(claim.verified);
        assert_eq!(claim.receipt_citations, vec![receipt.id.clone()]);
        assert_eq!(claim.confidence, 1.0);
    }

    #[test]
    fn syscall_topic_maps_only_the_two_verified_opcodes() {
        assert_eq!(
            SyscallOp::Infer.plane_bus_topic(),
            Some(crate::plane_bus::topics::GEMI_INFER_GENERATE)
        );
        assert_eq!(
            SyscallOp::ToolCall.plane_bus_topic(),
            Some(crate::plane_bus::topics::TOOLS_EXECUTE)
        );
        for unmapped in [
            SyscallOp::ContextAlloc,
            SyscallOp::BlackboardPost,
            SyscallOp::BlackboardRead,
            SyscallOp::ConsensusVote,
            SyscallOp::ReflexMount,
            SyscallOp::AuditSeal,
            SyscallOp::Heartbeat,
            SyscallOp::TelemetryGet,
        ] {
            assert_eq!(unmapped.plane_bus_topic(), None, "{unmapped:?}");
        }
    }

    fn syscall_req(op: SyscallOp) -> SyscallRequest {
        SyscallRequest {
            id: "req-1".into(),
            caller_id: "test".into(),
            op,
            token: None,
            workspace: None,
            payload: serde_json::json!({}),
            timestamp: 0,
        }
    }

    #[test]
    fn dispatch_syscall_reports_not_found_for_unmapped_opcodes() {
        let resp = dispatch_syscall(&syscall_req(SyscallOp::Heartbeat));
        assert_eq!(resp.id, "req-1");
        assert_eq!(resp.status, SyscallStatus::NotFound);
        assert!(resp.message.unwrap().contains("Heartbeat"));
    }

    #[test]
    fn wrap_syscall_result_maps_ok_and_err_correctly() {
        let ok = wrap_syscall_result(
            "id-1",
            Ok(serde_json::json!({"x": 1})),
            std::time::Duration::from_micros(5),
        );
        assert_eq!(ok.id, "id-1");
        assert_eq!(ok.status, SyscallStatus::Success);
        assert_eq!(ok.data, serde_json::json!({"x": 1}));
        assert_eq!(ok.latency_us, 5);
        assert!(ok.message.is_none());

        let err = wrap_syscall_result(
            "id-2",
            Err("boom".to_string()),
            std::time::Duration::from_micros(9),
        );
        assert_eq!(err.status, SyscallStatus::Error);
        assert_eq!(err.data, serde_json::Value::Null);
        assert_eq!(err.latency_us, 9);
        assert_eq!(err.message.as_deref(), Some("boom"));
    }
}
