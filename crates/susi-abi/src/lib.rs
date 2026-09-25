#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! # SUSI Swarm OS ABI
//!
//! Universal 0-dependency Application Binary Interface (ABI) for the SUSI Swarm OS.
//!
//! Provides language-agnostic syscall specifications, stigmergic pheromone schemas,
//! cryptographically verifiable evidence receipts, and zero-overhead IPC wire framing.
//!
//! Designed with **zero compile-time dependencies** on any other SUSI workspace crate,
//! enabling true decoupled microkernel and distributed swarm operation.

pub mod cell;
pub mod evidence;
pub mod router;
pub mod swarm;
pub mod syscall;
pub mod wire;

pub use cell::{CellState, SwarmCell};
pub use evidence::{GroundedClaim, ReceiptStatus, ToolReceipt};
pub use router::{PheromoneRouter, RouteCandidate, CELL_HEARTBEAT_TIMEOUT_SECS};
pub use swarm::{CapabilityBloom, PheromoneKind, SwarmCellManifest, SwarmPheromone, SwarmRole};
pub use syscall::{SyscallOp, SyscallRequest, SyscallResponse, SyscallStatus};
pub use wire::{MessageType, WireError, WireFrame, SUSI_WIRE_MAGIC, SUSI_WIRE_VERSION};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wire_frame_encode_decode_roundtrip() {
        let payload = br#"{"op":"infer","prompt":"hello world"}"#.to_vec();
        let frame = WireFrame::new(MessageType::SyscallRequest, payload.clone());
        let encoded = frame.encode();

        assert_eq!(&encoded[0..4], &SUSI_WIRE_MAGIC);
        assert_eq!(encoded[4], SUSI_WIRE_VERSION);
        assert_eq!(encoded[5], MessageType::SyscallRequest as u8);

        let (decoded, consumed) = WireFrame::decode(&encoded).expect("decode failed");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.version, SUSI_WIRE_VERSION);
        assert_eq!(decoded.msg_type, MessageType::SyscallRequest);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_wire_frame_incomplete_header() {
        let short_buf = *b"SUS";
        let err = WireFrame::decode(&short_buf).expect_err("should fail");
        assert_eq!(err, WireError::IncompleteHeader);
    }

    #[test]
    fn test_wire_frame_invalid_magic() {
        let bad_magic = [b'N', b'O', b'P', b'E', 1, 1, 0, 0, 0, 0, 0, 0];
        let err = WireFrame::decode(&bad_magic).expect_err("should fail");
        assert_eq!(err, WireError::InvalidMagic(*b"NOPE"));
    }

    #[test]
    fn test_syscall_opcode_conversion() {
        assert_eq!(SyscallOp::from_opcode(0x01), Some(SyscallOp::Infer));
        assert_eq!(SyscallOp::from_opcode(0x02), Some(SyscallOp::ToolCall));
        assert_eq!(SyscallOp::from_opcode(0x06), Some(SyscallOp::ConsensusVote));
        assert_eq!(SyscallOp::from_opcode(0xFF), None);
    }

    #[test]
    fn test_capability_bloom_filter() {
        let mut bloom = CapabilityBloom::empty();
        assert!(!bloom.may_contain_hash(12345));

        bloom.insert_hash(12345);
        assert!(bloom.may_contain_hash(12345));
    }

    #[test]
    fn test_tool_receipt_serialization() {
        let receipt = ToolReceipt {
            receipt_id: "rcpt-001".to_string(),
            tool_name: "exec_command".to_string(),
            args_hash: "abcd1234".to_string(),
            output_hash: "ef015678".to_string(),
            status: ReceiptStatus::Success,
            duration_us: 1500,
            observed_at: 1790333000,
            summary: Some("exit code 0".to_string()),
        };

        let json = serde_json::to_string(&receipt).expect("serialize failed");
        let deserialized: ToolReceipt = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(receipt, deserialized);
    }
}
