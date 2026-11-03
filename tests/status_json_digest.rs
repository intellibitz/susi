#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi status --json` reports the executable's digest so release automation
//! can verify installed identity across promotion and rollback
//! (T-RELEASESTATUS-1). The digest must be measured from the executable's
//! bytes on every call: the daemon's `binary.hash.cache` is keyed only by
//! mtime and size, so a same-size replacement with a restored timestamp, or a
//! hand-edited cache line, would otherwise report a stale or forged digest.
//! This test seeds a cache line that matches the binary's mtime and size but
//! carries a wrong digest, and asserts the command still reports the truth.

use std::path::{Path, PathBuf};
use std::process::Command;
use susi_paths::test_env::isolate_home;

fn scratch(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("susi-status-digest-{tag}-{}", std::process::id()))
}

fn true_sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn json_status_hashes_the_executable_even_when_a_matching_cache_line_lies() {
    let executable = Path::new(env!("CARGO_BIN_EXE_susi"));
    let home = scratch("forged-cache");
    let root = home.join(".susi-dev");
    let cwd = home.join("cwd");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    // A cache line that matches this binary's mtime and size exactly, so any
    // mtime:size-keyed lookup would accept it, with a digest that is wrong.
    let meta = std::fs::metadata(executable).unwrap();
    let mtime_ns = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let forged = "0".repeat(64);
    std::fs::write(
        root.join("binary.hash.cache"),
        format!("{mtime_ns}:{}:{forged}", meta.len()),
    )
    .unwrap();

    let mut command = Command::new(executable);
    isolate_home(&mut command, &home)
        .args(["status", "--json"])
        .current_dir(&cwd)
        .env("SUSI_HOME", &root);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "susi-status-v1");
    let reported = value["executable"]["sha256"].as_str().unwrap();
    assert_ne!(reported, forged, "status --json trusted the hash cache");
    assert_eq!(reported, true_sha256(executable));
    assert_eq!(value["executable"]["bytes"], meta.len());
    std::fs::remove_dir_all(home).unwrap();
}
