#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! D-SP-4 dependency invariant: the crates the thin `susi` client links
//! resolve to an exact, reviewed package set on both release targets
//! (x86_64 Windows and Linux). The graph is resolved locked and offline,
//! like `tests/zc_matrix.rs`, and needs no cross-target compiler.
//!
//! Each allow-list names packages without versions, so routine patch bumps
//! do not churn it; Cargo.lock and cargo-deny own versions. Later D-SP-4
//! stages add one allow-list per thin-client crate here. Any addition to a
//! list widens what the thin client links and needs review.

use std::collections::BTreeSet;
use std::process::Command;

/// Release targets core ships and tests.
const TARGETS: [&str; 2] = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"];

/// The std-only wire client: itself plus the JSON facade's closure.
const ALLOWED_WIRE_CLIENT: &[&str] = &[
    "susi-wire-client",
    "susi-vendor-serde-json",
    "serde_json",
    "serde_core",
    "itoa",
    "memchr",
    "zmij",
];

/// Package names in the normal and build graph of `package` on `target`.
/// A failing `cargo tree` fails the test with its stderr; it never skips.
fn closure(package: &str, target: &str) -> BTreeSet<String> {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--target",
            target,
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--locked",
            "--offline",
            "-p",
            package,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo tree -p {package} --target {target}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Each line is `name vX.Y.Z [(path)] [(*)]`: the first word is the name.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

fn assert_closure(package: &str, allowed: &[&str]) {
    let allowed: BTreeSet<String> = allowed.iter().map(|p| (*p).to_string()).collect();
    for target in TARGETS {
        let actual = closure(package, target);
        let added: Vec<_> = actual.difference(&allowed).collect();
        let removed: Vec<_> = allowed.difference(&actual).collect();
        assert!(
            added.is_empty() && removed.is_empty(),
            "{package} on {target} links packages outside its reviewed allow-list \
             (added {added:?}, gone {removed:?}); update the list only after review"
        );
    }
}

#[test]
fn wire_client_links_only_std_and_json() {
    assert_closure("susi-wire-client", ALLOWED_WIRE_CLIENT);
}

/// The conformance suite reuses the wire client's HTTP framing and stays a
/// black-box std + JSON client of the host contract.
#[test]
fn conformance_suite_links_only_the_wire_client_and_json() {
    let mut allowed = ALLOWED_WIRE_CLIENT.to_vec();
    allowed.push("susi-conformance");
    assert_closure("susi-conformance", &allowed);
}
