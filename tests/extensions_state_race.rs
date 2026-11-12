//! A pack load survives concurrent catalog readers in other processes.
//!
//! Every core catalog read re-derives `extensions/state.json` and wrote it
//! back without a cross-process lock, so a `susi extensions list` that read
//! the state just before `susi extensions load` wrote it could overwrite the
//! load: the operator was told the pack loaded, yet the active pack went back
//! to `default` and its pinned digests were dropped. This drives that race
//! with real processes under an isolated home across repeated iterations.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Failed setup or a lost load must fail this integration test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

const PACK: &str = "race-pack";
/// Other installed packs: each is one more manifest every reader's discovery
/// reads between taking the state and writing it back, which is how a real
/// install with several packs widens the window the lost load needed.
const FILLER_PACKS: usize = 24;
const ITERATIONS: usize = 6;
const READERS: usize = 4;

fn bin() -> &'static str {
    option_env!("CARGO_BIN_EXE_susi").unwrap_or("susi")
}

fn run(home: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin());
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.contains("KEY") || name.contains("TOKEN") || name.contains("SECRET") {
            cmd.env_remove(key);
        }
    }
    susi_paths::test_env::isolate_home(&mut cmd, home)
        .env("SUSI_HOME", home)
        .env_remove("SUSI_EXTENSION_PACK")
        .args(args)
        .output()
        .unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    susi_paths::hex::encode(Sha256::digest(bytes))
}

/// Install `id` with a checksummed catalog; returns its manifest digest.
fn install_pack(extensions: &Path, id: &str) -> String {
    let root = extensions.join(id);
    std::fs::create_dir_all(&root).unwrap();
    let catalog = format!(
        r#"[{{"id":"{id}-vendor","aliases":["{id}-vendor"],"api_key_env":"RACE_API_KEY"}}]"#
    );
    std::fs::write(root.join("cloud-vendors.json"), &catalog).unwrap();
    let manifest = serde_json::json!({
        "id": id,
        "name": format!("Race fixture {id}"),
        "version": "1.0.0",
        "apiVersion": "1",
        "capabilities": ["catalog.cloud_vendors"],
        "permissions": [],
        "files": {"cloud-vendors.json": "cloud-vendors.json"},
        "checksums": {"cloud-vendors.json": format!("sha256:{}", sha256_hex(catalog.as_bytes()))}
    });
    let text = serde_json::to_string_pretty(&manifest).unwrap();
    std::fs::write(root.join("manifest.json"), &text).unwrap();
    format!("sha256:{}", sha256_hex(text.as_bytes()))
}

fn scratch_home(iteration: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "susi-ext-race-{}-{}-{iteration}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

fn template_extensions() -> &'static (PathBuf, String) {
    static TEMPLATE: OnceLock<(PathBuf, String)> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let root = std::env::temp_dir().join(format!(
            "susi-ext-race-template-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let extensions = root.join("extensions");
        for n in 0..FILLER_PACKS {
            install_pack(&extensions, &format!("filler-{n:02}"));
        }
        let manifest_digest = install_pack(&extensions, PACK);
        (extensions, manifest_digest)
    })
}

#[test]
fn a_pack_load_stays_active_and_pinned_under_concurrent_lists() {
    let (tmpl_ext, manifest_digest) = template_extensions();
    for iteration in 0..ITERATIONS {
        let home = scratch_home(iteration);
        let extensions = home.join("extensions");
        copy_dir_all(tmpl_ext, &extensions);

        let stop = AtomicBool::new(false);
        let started = AtomicUsize::new(0);
        let failures = std::sync::Mutex::new(Vec::<String>::new());
        let loaded = std::thread::scope(|scope| {
            for _ in 0..READERS {
                scope.spawn(|| {
                    let mut runs = 0usize;
                    // Keep reading until told to stop, and at least twice
                    // after the load returned, so readers that took the
                    // state just before the load wrote it finish after it.
                    let mut after_stop = 0usize;
                    while after_stop < 2 {
                        if stop.load(Ordering::SeqCst) {
                            after_stop += 1;
                        }
                        let out = run(&home, &["extensions", "list"]);
                        if !out.status.success() {
                            failures.lock().unwrap().push(format!("{out:?}"));
                        }
                        runs += 1;
                        if runs == 1 {
                            started.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
            }
            while started.load(Ordering::SeqCst) < READERS {
                std::thread::yield_now();
            }
            let loaded = run(&home, &["extensions", "load", PACK]);
            stop.store(true, Ordering::SeqCst);
            loaded
        });
        assert!(
            loaded.status.success(),
            "iteration {iteration}: load failed: {loaded:?}"
        );
        let reported: serde_json::Value = serde_json::from_slice(&loaded.stdout).unwrap();
        assert_eq!(
            reported["active"], PACK,
            "iteration {iteration}: load reported {reported}"
        );
        let failures = failures.into_inner().unwrap();
        assert!(
            failures.is_empty(),
            "iteration {iteration}: concurrent lists failed: {failures:?}"
        );

        // The load the operator was told succeeded is still in force.
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(extensions.join("state.json")).unwrap()).unwrap();
        assert_eq!(
            state["active"], PACK,
            "iteration {iteration}: a concurrent reader overwrote the load: {state}"
        );
        assert_eq!(
            state["trusted_digests"][PACK]["manifest.json"],
            manifest_digest.as_str(),
            "iteration {iteration}: the pinned digests were dropped: {state}"
        );
        assert!(
            state["trusted_digests"][PACK]["cloud-vendors.json"].is_string(),
            "iteration {iteration}: the catalog pin was dropped: {state}"
        );
        assert!(
            state["loaded"]
                .as_array()
                .is_some_and(|l| l.iter().any(|x| x == PACK)),
            "iteration {iteration}: {state}"
        );
        assert!(
            state["quarantined"]
                .get(PACK)
                .is_none_or(|q| q.as_object().is_some_and(|m| m.is_empty())),
            "iteration {iteration}: {state}"
        );
        let status = run(&home, &["extensions", "status"]);
        assert!(status.status.success(), "{status:?}");
        let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["active"], PACK, "iteration {iteration}: {status}");
        // Every process released the state lock on its way out.
        assert!(
            !extensions.join(".state.lock").exists(),
            "iteration {iteration}: the state lock was left held"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[test]
fn unit_sha256_hex_computes_deterministic_digest() {
    let d1 = sha256_hex(b"cloud-vendors-payload");
    let d2 = sha256_hex(b"cloud-vendors-payload");
    assert_eq!(d1.len(), 64);
    assert_eq!(d1, d2);
    assert_ne!(d1, sha256_hex(b"different-payload"));
}

#[test]
fn unit_install_pack_writes_manifest_and_cloud_vendors_with_matching_checksum() {
    let home = scratch_home(999_001);
    let extensions = home.join("extensions");
    let digest = install_pack(&extensions, "unit-pack");
    assert!(digest.starts_with("sha256:"));
    let manifest_bytes = std::fs::read(extensions.join("unit-pack").join("manifest.json")).unwrap();
    assert_eq!(digest, format!("sha256:{}", sha256_hex(&manifest_bytes)));
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest["id"], "unit-pack");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn unit_scratch_home_generates_distinct_paths_per_iteration() {
    let h1 = scratch_home(1);
    let h2 = scratch_home(2);
    assert_ne!(h1, h2);
}

#[test]
fn unit_bin_and_sha256_helpers_produce_valid_outputs() {
    assert!(!bin().is_empty());
    let empty_digest = sha256_hex(b"");
    assert_eq!(empty_digest.len(), 64);
    assert_ne!(empty_digest, sha256_hex(b"susi"));
}
