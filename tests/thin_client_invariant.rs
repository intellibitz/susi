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

/// Packages in `susi-cli-defs`'s `clap` closure, the same on both targets.
/// `susi-vendor-clap` builds clap without default features (T-CARBONLEAN-12),
/// so the terminal colour stack (`anstream` and its Windows console packages)
/// is not linked; `anstyle` stays as a dependency of `clap_builder`.
const ALLOWED_CLI_DEFS: &[&str] = &[
    "anstyle",
    "clap",
    "clap_builder",
    "clap_derive",
    "clap_lex",
    "heck",
    "idna_adapter",
    "proc-macro2",
    "quote",
    "strsim",
    "susi-cli-defs",
    "susi-vendor-clap",
    "syn",
    "unicode-ident",
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

fn assert_closure_on_target(package: &str, target: &str, allowed: &[&str]) {
    let allowed: BTreeSet<String> = allowed.iter().map(|p| (*p).to_string()).collect();
    let actual = closure(package, target);
    let added: Vec<_> = actual.difference(&allowed).collect();
    let removed: Vec<_> = allowed.difference(&actual).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "{package} on {target} links packages outside its reviewed allow-list \
         (added {added:?}, gone {removed:?}); update the list only after review"
    );
}

fn assert_closure(package: &str, allowed: &[&str]) {
    for target in TARGETS {
        assert_closure_on_target(package, target, allowed);
    }
}

#[test]
fn wire_client_links_only_std_and_json() {
    assert_closure("susi-wire-client", ALLOWED_WIRE_CLIENT);
}

#[test]
fn cli_defs_links_only_the_clap_closure() {
    assert_closure("susi-cli-defs", ALLOWED_CLI_DEFS);
}

/// The conformance suite reuses the wire client's HTTP framing and stays a
/// black-box std + JSON client of the host contract.
#[test]
fn conformance_suite_links_only_the_wire_client_and_json() {
    let mut allowed = ALLOWED_WIRE_CLIENT.to_vec();
    allowed.push("susi-conformance");
    assert_closure("susi-conformance", &allowed);
}

#[test]
fn unit_targets_list_exactly_windows_and_linux_x86_64() {
    assert_eq!(
        TARGETS,
        ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
    );
}

#[test]
fn unit_allowed_wire_client_excludes_async_and_http_runtimes() {
    assert!(ALLOWED_WIRE_CLIENT.contains(&"susi-wire-client"));
    assert!(ALLOWED_WIRE_CLIENT.contains(&"serde_json"));
    assert!(!ALLOWED_WIRE_CLIENT.contains(&"tokio"));
    assert!(!ALLOWED_WIRE_CLIENT.contains(&"reqwest"));
    assert!(!ALLOWED_WIRE_CLIENT.contains(&"hyper"));
}
