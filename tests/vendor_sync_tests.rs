#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Shared contracts are mounted from canonical source files. These tests make
//! physical copies a regression: a consumer must compile the canonical source
//! through `#[path]`, keeping one implementation without a Cargo crate edge.

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
        for module in ["susi_core", "susi_sandbox", "susi_native"] {
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
        "shared contracts must use canonical #[path] mounts; duplicate paths: {duplicates:?}"
    );
}

#[test]
fn every_declared_source_mount_resolves() {
    let root = workspace_root();
    let mut pending = vec![root.join("crates"), root.join("src"), root.join("tests")];
    let mut checked = 0_usize;

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read Rust source");
            for line in source.lines() {
                let Some(rest) = line.split_once("#[path = \"").map(|(_, rest)| rest) else {
                    continue;
                };
                let (mount, _) = rest.split_once('\"').expect("well-formed path attribute");
                checked += 1;
                let target = path.parent().expect("source parent").join(mount);
                assert!(
                    target.is_file(),
                    "source mount in {} does not resolve: {}",
                    path.display(),
                    target.display()
                );
            }
        }
    }

    assert!(
        checked > 100,
        "expected broad canonical source reuse, got {checked}"
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
        "byte-identical Rust files must be consolidated through a canonical source mount: {duplicates:?}"
    );
}
