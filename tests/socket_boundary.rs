//! CEO rule (2026-11-04): core is the free open-source socket. It does not
//! gain third-party product implementations. A third party writes the plug.
//! The crates below still contain that debt; their Rust source may only shrink.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn third_party_product_implementations_only_shrink() {
    let root = workspace_root();
    // Ceilings are the src/**/*.rs file counts on 2026-11-04. Lower them when
    // a plug leaves core. Do not raise them.
    let ceilings = [
        ("crates/susi-vendor-agents/src", 10usize),
        ("crates/susi-vendor-models/src", 61),
        ("crates/susi-vendor-cloud/src", 3),
        ("crates/susi-vendor-chrome/src", 1),
        ("crates/susi-vendor-web/src", 2),
        ("crates/susi-vendor-candle/src", 4),
    ];
    for (rel, ceiling) in ceilings {
        let mut files = Vec::new();
        rust_files(&root.join(rel), &mut files);
        assert!(
            files.len() <= ceiling,
            "{rel} has {} Rust files; the ceiling is {ceiling}. Core does not gain third-party product implementations",
            files.len()
        );
    }
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).expect("read AGENTS.md");
    assert!(
        agents.contains("open-source socket"),
        "AGENTS.md must keep the open-source socket rule"
    );
    assert!(
        agents.contains("A third party writes the"),
        "AGENTS.md must keep the third-party plug rule"
    );
}
