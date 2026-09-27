#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Shared code has exactly one checked-in implementation. These tests make
//! physical copies a regression: a consumer reaches shared code through a
//! Cargo edge on the owning crate (see `architecture_tests.rs` for the leaf
//! order and the cross-crate `#[path]` mount ratchet).

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn shared_contracts_have_no_consumer_copies() {
    let root = workspace_root();
    let crates = root.join("crates");
    let mut duplicates = Vec::new();

    for entry in std::fs::read_dir(&crates).expect("read crates directory") {
        let crate_dir = entry.expect("read crate entry").path();
        if crate_dir.file_name().and_then(|name| name.to_str()) == Some("susi-core") {
            continue;
        }
        let src = crate_dir.join("src");
        for module in ["susi_core", "susi_sandbox"] {
            let path = src.join(module);
            if path.exists() {
                duplicates.push(path);
            }
        }
        for module in ["susi_error.rs", "susi_paths.rs", "susi_config.rs"] {
            let path = src.join(module);
            if path.exists() {
                duplicates.push(path);
            }
        }
    }

    assert!(
        duplicates.is_empty(),
        "shared contracts must not be copied into consumers; duplicate paths: {duplicates:?}"
    );
}

#[test]
fn checked_in_rust_implementations_are_not_byte_duplicates() {
    let root = workspace_root();
    let mut pending = vec![
        root.join("crates"),
        root.join("src"),
        root.join("tests"),
        root.join("xtask"),
    ];
    let mut unique = std::collections::HashMap::<Vec<u8>, PathBuf>::new();
    let mut duplicates = Vec::new();

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                let source = std::fs::read(&path).expect("read Rust source");
                if let Some(first) = unique.insert(source, path.clone()) {
                    duplicates.push((first, path));
                }
            }
        }
    }

    assert!(
        duplicates.is_empty(),
        "byte-identical Rust files must be consolidated into one owning crate: {duplicates:?}"
    );
}
