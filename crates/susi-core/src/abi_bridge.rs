//! Bridge from susi-core's internal receipt bookkeeping to the shared
//! `susi-abi` wire vocabulary.
//!
//! Kept out of `capture.rs` on purpose: that file is vendored byte-identical
//! into 9 zero-dependency consumer crates (`scripts/check-vendored-sync.sh`),
//! and susi-core is currently the only crate with a real Cargo edge to
//! `susi-abi`. This file stays outside the vendored `src/susi_core/` tree so
//! that edge never leaks into the vendored copies.

use crate::capture::ToolReceipt;
use susi_abi::evidence::ReceiptStatus;

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
}
