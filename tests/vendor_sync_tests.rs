#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Vendored `susi_core` drift check: every `crates/*/src/susi_core/<file>.rs`
//! (and the vendor template) must be byte-identical to the canonical
//! `crates/susi-core/src/<file>.rs`. The canonical crate self-aliases as
//! `susi_core`, so `crate::susi_core::…` paths resolve identically in both
//! contexts — no path rewriting or test stripping is permitted. `mod.rs`
//! is exempt: each consumer declares its own subset.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn vendored_susi_core_files_are_byte_identical_to_canonical() {
    let root = workspace_root();
    let canon_dir = root.join("crates/susi-core/src");
    let mut checked = 0usize;
    let mut drift = Vec::new();

    let mut vendor_dirs = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("read crates/") {
        let dir = entry.expect("dir entry").path().join("src/susi_core");
        if dir.is_dir() {
            vendor_dirs.push(dir);
        }
    }
    vendor_dirs.push(root.join("crates/susi-core/vendor_template/susi_core"));

    for dir in vendor_dirs {
        for entry in std::fs::read_dir(&dir).expect("read vendored dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("mod.rs") {
                continue;
            }
            let canon = canon_dir.join(path.file_name().expect("file name"));
            if !canon.exists() {
                drift.push(format!("{}: no canonical counterpart", path.display()));
                continue;
            }
            checked += 1;
            let vendored = std::fs::read(&path).expect("read vendored file");
            let canonical = std::fs::read(&canon).expect("read canonical file");
            if vendored != canonical {
                drift.push(format!("{}: differs from canonical", path.display()));
            }
        }
    }

    assert!(
        checked > 100,
        "expected to check many vendored files, got {checked}"
    );
    assert!(
        drift.is_empty(),
        "vendored susi_core drift detected — re-sync from crates/susi-core/src:\n{}",
        drift.join("\n")
    );
}
