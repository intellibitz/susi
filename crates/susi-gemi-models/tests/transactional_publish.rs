// Fixtures and assertions fail loudly; production retains the denied panic lint.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use susi_gemi_models::resumable_downloads::{sha256_hex, DownloadPlan, RangeResponse};
use susi_gemi_models::transactional_download::Store;

const NAME: &str = "model.gguf";
const DATA: &[u8] = b"verified replacement payload";

fn plan() -> DownloadPlan {
    DownloadPlan {
        url: "https://example.invalid/model.gguf?token=synthetic-test-only".into(),
        dest: PathBuf::new(),
        sha256: Some(sha256_hex(DATA)),
        size: Some(DATA.len() as u64),
        max_retries: 0,
    }
}

fn publish(
    store: &Store,
) -> susi_error::EaiResult<susi_gemi_models::transactional_download::PublishReport> {
    store.publish(NAME, &plan(), &|_| {
        Ok(RangeResponse {
            status: 200,
            body: DATA.to_vec(),
            accept_ranges: true,
        })
    })
}

#[test]
fn publication_without_a_declared_digest_is_not_reported_verified() {
    let root = susi_paths::tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let mut unverified = plan();
    unverified.sha256 = None;
    let report = store
        .publish(NAME, &unverified, &|_| {
            Ok(RangeResponse {
                status: 200,
                body: DATA.to_vec(),
                accept_ranges: true,
            })
        })
        .unwrap();
    assert!(report.published);
    assert!(!report.verified);
}

#[test]
fn range_producer_replaces_old_payload_and_invalidates_old_provenance() {
    let root = susi_paths::tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let models = root.path().join("models");
    fs::create_dir_all(&models).unwrap();
    fs::write(models.join(NAME), b"old").unwrap();
    fs::write(models.join("model.gguf.manifest.json"), b"old manifest").unwrap();
    fs::write(models.join("model.provenance.json"), b"old identity").unwrap();
    fs::create_dir_all(root.path().join("staging")).unwrap();
    fs::write(
        root.path().join("staging/model.gguf.provenance.json"),
        b"unbound stale sidecar",
    )
    .unwrap();
    assert!(store.is_ready(NAME));
    let report = publish(&store).unwrap();
    assert!(report.published && report.verified && store.is_ready(NAME));
    assert_eq!(fs::read(models.join(NAME)).unwrap(), DATA);
    assert!(!models.join("model.provenance.json").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(models.join("model.gguf.manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["sha256"], sha256_hex(DATA));
    assert_eq!(manifest["size"], DATA.len());
    assert!(manifest.get("source_url").is_none());
}

#[test]
fn failed_artifact_rename_cannot_leave_stale_readiness() {
    let root = susi_paths::tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let models = root.path().join("models");
    // A directory cannot be overwritten by the staged file on supported hosts.
    fs::create_dir_all(models.join(NAME)).unwrap();
    fs::write(models.join("model.gguf.manifest.json"), b"old manifest").unwrap();
    assert!(publish(&store).is_err());
    assert!(!models.join("model.gguf.manifest.json").exists());
    assert!(!store.is_ready(NAME));
    assert_eq!(store.recover().len(), 1);
    assert_eq!(fs::read(&store.recover()[0].staged).unwrap(), DATA);
}

#[test]
fn readiness_invalidation_failure_does_not_replace_existing_bytes() {
    let root = susi_paths::tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let models = root.path().join("models");
    fs::create_dir_all(models.join("model.gguf.manifest.json")).unwrap();
    fs::write(models.join(NAME), b"old bytes").unwrap();
    assert!(publish(&store).is_err());
    assert_eq!(fs::read(models.join(NAME)).unwrap(), b"old bytes");
    assert!(!store.is_ready(NAME));
    assert_eq!(store.recover().len(), 1);
}

#[test]
fn interrupted_range_transfer_preserves_previous_ready_version() {
    let root = susi_paths::tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    publish(&store).unwrap();
    let result = store.publish(NAME, &plan(), &|_| {
        Err(susi_error::EaiError::network("injected interruption"))
    });
    assert!(result.is_err());
    assert!(store.is_ready(NAME));
    assert_eq!(
        fs::read(root.path().join("models").join(NAME)).unwrap(),
        DATA
    );
}

#[cfg(windows)]
#[test]
fn provenance_rename_failure_rolls_payload_back_to_staging() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = susi_paths::tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let staged = store.staging_target(NAME);
    fs::create_dir_all(staged.parent().unwrap()).unwrap();
    fs::write(&staged, DATA).unwrap();
    let provenance = staged.with_extension("provenance.json");
    fs::write(&provenance, b"new identity").unwrap();
    // Deny rename of this sidecar, deterministically after payload promotion.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&provenance)
        .unwrap();
    let error = store.commit_staged(NAME, &plan(), &|_| true).unwrap_err();
    assert!(error.to_string().contains("publish model provenance"));
    assert_eq!(fs::read(&staged).unwrap(), DATA);
    assert!(!store.is_ready(NAME));
    assert!(!root.path().join("models").join(NAME).exists());
    drop(held);
    assert!(
        store
            .commit_staged(NAME, &plan(), &|_| true)
            .unwrap()
            .published
    );
    assert!(store.is_ready(NAME));
}
