//! Transactional model download publishing (VC-201-044).

use crate::resumable_downloads::{sha256_hex, DownloadPlan, RangeResponse};
use crate::susi_error::EaiResult;
use crate::transactional_download::Store;
use std::path::{Path, PathBuf};

const DATA: &[u8] = b"gguf-payload-transactional-test-0123456789";

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("susi-txd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn plan(url: &str, sha: Option<String>) -> DownloadPlan {
    DownloadPlan {
        url: url.to_string(),
        dest: PathBuf::new(), // overwritten by publish → staging
        sha256: sha,
        size: Some(DATA.len() as u64),
        max_retries: 4,
    }
}

/// Scriptable transport: queued responses, then errors forever.
struct Script {
    responses: std::cell::RefCell<std::collections::VecDeque<EaiResult<RangeResponse>>>,
}

impl Script {
    fn of(responses: Vec<EaiResult<RangeResponse>>) -> Self {
        Self {
            responses: std::cell::RefCell::new(responses.into()),
        }
    }
    fn call(&self, _req: &crate::resumable_downloads::RangeRequest) -> EaiResult<RangeResponse> {
        self.responses
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(crate::susi_error::EaiError::network("script exhausted")))
    }
}

fn partial(body: &[u8]) -> EaiResult<RangeResponse> {
    Ok(RangeResponse {
        status: 206,
        body: body.to_vec(),
        accept_ranges: true,
    })
}

#[test]
fn vc_201_044_verified_artifact_publishes_atomically() {
    let root = dir("pub");
    let store = Store::new(&root);
    let script = Script::of(vec![partial(DATA)]);
    let r = store
        .publish(
            "m.gguf",
            &plan("http://x/m", Some(sha256_hex(DATA))),
            &|q| script.call(q),
        )
        .unwrap();
    assert!(r.published && r.verified);
    assert!(store.is_ready("m.gguf"));
    assert_eq!(std::fs::read(r.path.unwrap()).unwrap(), DATA);
    assert!(store.recover().is_empty(), "nothing left staged");
}

#[test]
fn vc_201_044_checksum_mismatch_never_appears_ready() {
    let root = dir("bad");
    let store = Store::new(&root);
    let script = Script::of(vec![partial(DATA)]);
    let r = store
        .publish("m.gguf", &plan("http://x/m", Some("00".repeat(32))), &|q| {
            script.call(q)
        })
        .unwrap();
    assert!(!r.published && !r.verified);
    assert!(!store.is_ready("m.gguf"));
    // Quarantined, not staged-and-confusable.
    assert!(store.recover().is_empty());
    assert!(
        root.join("staging").join("m.gguf.corrupt").exists()
            || !root.join("staging").join("m.gguf.part").exists()
    );
}

#[test]
fn vc_201_044_interrupted_download_recovers_and_resumes() {
    let root = dir("cut");
    let store = Store::new(&root);
    // First attempt: one partial response then a permanent transport error.
    let script = Script::of(vec![partial(&DATA[..20])]);
    let err = store
        .publish(
            "m.gguf",
            &plan("http://x/m", Some(sha256_hex(DATA))),
            &|q| script.call(q),
        )
        .unwrap_err();
    assert!(err.to_string().contains("retries") || err.to_string().contains("exhausted"));
    assert!(!store.is_ready("m.gguf"));
    // Recoverable state: the staged bytes survive.
    let staged = store.recover();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].bytes, 20);
    // Second attempt: tail only — resumes from the staged 20 bytes.
    let script2 = Script::of(vec![partial(&DATA[20..])]);
    let r = store
        .publish(
            "m.gguf",
            &plan("http://x/m", Some(sha256_hex(DATA))),
            &|q| script2.call(q),
        )
        .unwrap();
    assert!(r.published && r.verified && r.resumed_from == 20);
    assert!(store.is_ready("m.gguf"));
}

#[test]
fn vc_201_044_manifest_written_only_after_artifact() {
    let root = dir("mani");
    let store = Store::new(&root);
    let script = Script::of(vec![partial(DATA)]);
    store
        .publish(
            "m.gguf",
            &plan("http://x/m", Some(sha256_hex(DATA))),
            &|q| script.call(q),
        )
        .unwrap();
    let manifest = std::fs::read_to_string(root.join("models/m.gguf.manifest.json")).unwrap();
    assert!(manifest.contains("sha256"));
    // A store with only an artifact and no manifest is not ready.
    std::fs::remove_file(root.join("models/m.gguf.manifest.json")).unwrap();
    assert!(!store.is_ready("m.gguf"));
}

#[test]
fn vc_201_044_disk_full_style_write_failure_leaves_recoverable_state() {
    let root = dir("full");
    let store = Store::new(&root);
    // Simulate a mid-write failure: remove staging dir write access isn't
    // portable — instead exhaust the script to force a transport error,
    // then prove recover() reports exactly the bytes that landed.
    let script = Script::of(vec![partial(&DATA[..15])]);
    let _ = store.publish("m.gguf", &plan("http://x/m", None), &|q| script.call(q));
    let staged = store.recover();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].bytes, 15);
    assert!(!store.is_ready("m.gguf"), "incomplete never reads as ready");
    let _p: &Path = Path::new("x");
}
