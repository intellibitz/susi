//! Vector VC-201-044 mastery tests.
//!
//! Vector: Publish model downloads transactionally.
//! Mastery target: Extend managed installation with resumable content
//! verification and staging; interrupted or checksum-mismatched downloads
//! never appear as ready models and disk-full failures leave recoverable state.

use crate::resumable_downloads::{sha256_hex, DownloadPlan, RangeRequest, RangeResponse};
use crate::susi_error::EaiResult;
use crate::transactional_download::Store;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

const PAYLOAD: &[u8] = b"transactional-model-mastery-payload-9876543210";

fn temp_store_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("susi-tx-mastery-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct MockTransport {
    chunks: RefCell<VecDeque<EaiResult<RangeResponse>>>,
}

impl MockTransport {
    fn new(chunks: Vec<EaiResult<RangeResponse>>) -> Self {
        Self {
            chunks: RefCell::new(chunks.into()),
        }
    }

    fn call(&self, _req: &RangeRequest) -> EaiResult<RangeResponse> {
        self.chunks
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(crate::susi_error::EaiError::network("stream interrupted")))
    }
}

fn range_ok(bytes: &[u8]) -> EaiResult<RangeResponse> {
    Ok(RangeResponse {
        status: 206,
        body: bytes.to_vec(),
        accept_ranges: true,
    })
}

#[test]
fn vc_201_044_mastery_checksum_mismatch_quarantines_and_never_ready() {
    let root = temp_store_dir("mismatch");
    let store = Store::new(&root);
    let transport = MockTransport::new(vec![range_ok(PAYLOAD)]);

    let plan = DownloadPlan {
        url: "http://mock/model.gguf".into(),
        dest: PathBuf::new(),
        sha256: Some("f".repeat(64)), // Deliberately bogus checksum
        size: Some(PAYLOAD.len() as u64),
        max_retries: 2,
    };

    let report = store
        .publish("model.gguf", &plan, &|req| transport.call(req))
        .unwrap();

    // Mismatched artifact must never report published or verified
    assert!(!report.published);
    assert!(!report.verified);

    // Store must never declare a mismatched model ready
    assert!(!store.is_ready("model.gguf"));

    // Staging must not leave corrupt bytes staged as active parts
    assert!(store.recover().is_empty());
}

#[test]
fn vc_201_044_mastery_interrupted_download_preserves_recoverable_state() {
    let root = temp_store_dir("recoverable");
    let store = Store::new(&root);

    // Transport delivers half the payload then fails
    let transport = MockTransport::new(vec![range_ok(&PAYLOAD[..20])]);

    let plan = DownloadPlan {
        url: "http://mock/model.gguf".into(),
        dest: PathBuf::new(),
        sha256: Some(sha256_hex(PAYLOAD)),
        size: Some(PAYLOAD.len() as u64),
        max_retries: 1,
    };

    let err = store
        .publish("model.gguf", &plan, &|req| transport.call(req))
        .unwrap_err();
    assert!(err.to_string().contains("retries") || err.to_string().contains("interrupted"));

    // Incomplete download must not be marked ready
    assert!(!store.is_ready("model.gguf"));

    // Recovered state must record exact partial bytes
    let recovered = store.recover();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].name, "model.gguf");
    assert_eq!(recovered[0].bytes, 20);

    // Resumed attempt provides remainder and succeeds
    let resume_transport = MockTransport::new(vec![range_ok(&PAYLOAD[20..])]);
    let report = store
        .publish("model.gguf", &plan, &|req| resume_transport.call(req))
        .unwrap();

    assert!(report.published);
    assert!(report.verified);
    assert_eq!(report.resumed_from, 20);
    assert!(store.is_ready("model.gguf"));
}

#[test]
fn vc_201_044_mastery_clean_single_shot_publish_verifies_and_marks_ready() {
    let root = temp_store_dir("single-shot");
    let store = Store::new(&root);
    let transport = MockTransport::new(vec![range_ok(PAYLOAD)]);

    let plan = DownloadPlan {
        url: "http://mock/model.gguf".into(),
        dest: PathBuf::new(),
        sha256: Some(sha256_hex(PAYLOAD)),
        size: Some(PAYLOAD.len() as u64),
        max_retries: 1,
    };

    let report = store
        .publish("model.gguf", &plan, &|req| transport.call(req))
        .unwrap();
    assert!(report.published && report.verified);
    assert_eq!(report.resumed_from, 0);
    assert!(store.is_ready("model.gguf"));
    assert!(store.recover().is_empty());
}

#[test]
fn vc_201_044_mastery_size_mismatch_quarantines_and_never_marks_ready() {
    let root = temp_store_dir("size-mismatch");
    let store = Store::new(&root);
    let transport = MockTransport::new(vec![range_ok(PAYLOAD)]);

    let plan = DownloadPlan {
        url: "http://mock/model.gguf".into(),
        dest: PathBuf::new(),
        sha256: Some(sha256_hex(PAYLOAD)),
        size: Some((PAYLOAD.len() + 10) as u64),
        max_retries: 0,
    };

    let _ = store.publish("model.gguf", &plan, &|req| transport.call(req));
    assert!(!store.is_ready("model.gguf"));
}

#[test]
fn vc_201_044_mastery_unverified_model_name_is_never_ready_on_fresh_store() {
    let root = temp_store_dir("fresh-empty");
    let store = Store::new(&root);
    assert!(!store.is_ready("nonexistent-model.gguf"));
    assert!(store.recover().is_empty());
}
