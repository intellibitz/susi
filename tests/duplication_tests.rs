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
        for module in ["susi_core", "susi_sandbox", "susi_abi"] {
            let path = src.join(module);
            if path.exists() {
                duplicates.push(path);
            }
        }
        for module in [
            "susi_error.rs",
            "susi_paths.rs",
            "susi_config.rs",
            "susi_abi.rs",
        ] {
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

/// `susi-core` is the microkernel every plane shares. Consumers reach it
/// through a Cargo edge (`pub use susi_core;`); a `#[path]` mount into its
/// tree would give that consumer its own types and `OnceLock` statics, and
/// the bus/registry/capture rendezvous would silently stop matching.
#[test]
fn susi_core_is_never_source_mounted_into_a_consumer() {
    let crates = workspace_root().join("crates");
    let mut offenders = Vec::new();
    let mut pending = vec![crates.clone()];

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read crate directory") {
            let path = entry.expect("read crate entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs")
                || path.starts_with(crates.join("susi-core"))
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read Rust source");
            if text.contains("#[path = \"../../susi-core/") {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "susi-core must be reached through a Cargo edge, not a source mount: {offenders:?}"
    );
}

/// `susi-abi` is the Swarm OS wire contract. Consumers reach it through a
/// Cargo edge (`pub use susi_abi;` or a direct `use`); a `#[path]` mount
/// into its tree would give that consumer its own ABI types.
#[test]
fn susi_abi_is_never_source_mounted_into_a_consumer() {
    let crates = workspace_root().join("crates");
    let mut offenders = Vec::new();
    let mut pending = vec![crates.clone()];

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read crate directory") {
            let path = entry.expect("read crate entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs")
                || path.starts_with(crates.join("susi-abi"))
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read Rust source");
            if text.contains("#[path = \"../../susi-abi/") {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "susi-abi must be reached through a Cargo edge, not a source mount: {offenders:?}"
    );
}

/// `susi-gawd-agents` is the fleet/detectors leaf. Consumers reach it
/// through a Cargo edge; remounting its sources would give each consumer
/// its own fleet types and detector statics.
#[test]
fn susi_gawd_agents_is_never_source_mounted_into_a_consumer() {
    let crates = workspace_root().join("crates");
    let mut offenders = Vec::new();
    let mut pending = vec![crates.clone()];

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read crate directory") {
            let path = entry.expect("read crate entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs")
                || path.starts_with(crates.join("susi-gawd-agents"))
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read Rust source");
            if text.contains("#[path = \"../../susi-gawd-agents/") {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "susi-gawd-agents must be reached through a Cargo edge, not a source mount: {offenders:?}"
    );
}

/// Within-plane crates (`susi-gawd-swarm`, `susi-gawd-a2a`, `susi-gemi-models`)
/// are reached through Cargo edges. Remounting their sources would give the
/// host crate a second copy of the same types.
#[test]
fn within_plane_crates_are_never_source_mounted_into_a_consumer() {
    let crates = workspace_root().join("crates");
    let mut offenders = Vec::new();
    let mut pending = vec![crates.clone()];
    let banned = [
        "#[path = \"../../susi-gawd-swarm/",
        "#[path = \"../../susi-gawd-a2a/",
        "#[path = \"../../susi-gemi-models/",
    ];

    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read crate directory") {
            let path = entry.expect("read crate entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let owned = path.starts_with(crates.join("susi-gawd-swarm"))
                || path.starts_with(crates.join("susi-gawd-a2a"))
                || path.starts_with(crates.join("susi-gemi-models"));
            if owned {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read Rust source");
            if banned.iter().any(|needle| text.contains(needle)) {
                offenders.push(path);
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "within-plane crates must be reached through a Cargo edge, not a source mount: {offenders:?}"
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
