#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Resolve the actual target dependency graph, not historical facade names.
//! Wasmi supplies Rust-only WebAssembly execution. Windows TLS uses Schannel;
//! Linux platform OpenSSL is an explicit OS boundary, not a pure Rust claim.
//! Graph resolution is locked and offline and needs no cross-target compiler.

use std::process::Command;

fn dep_tree(target: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--target",
            target,
            "-e",
            "normal",
            "--prefix",
            "none",
            "--locked",
            "--offline",
            "-p",
            "susi",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo tree --target {target}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn packages(tree: &str) -> std::collections::HashSet<&str> {
    tree.lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

fn assert_native_runtime_graph(tree: &str) {
    let packages = packages(tree);
    assert!(
        packages.contains("wasmi"),
        "actual Wasmi runtime missing: {tree}"
    );
    assert!(
        packages.contains("candle-core"),
        "Rust model runtime missing: {tree}"
    );
    for package in &packages {
        assert!(
            !matches!(
                *package,
                "wasmtime"
                    | "wasmer"
                    | "ort"
                    | "ort-sys"
                    | "onnxruntime"
                    | "ring"
                    | "aws-lc-rs"
                    | "aws-lc-sys"
                    | "rustls"
                    | "rustls-webpki"
                    | "webpki-roots"
            ) && !package.starts_with("wasmtime-")
                && !package.starts_with("wasmer-"),
            "retired runtime/crypto dependency {package} returned: {tree}"
        );
    }
}

#[test]
fn windows_resolves_rust_runtime_and_native_tls() {
    let tree = dep_tree("x86_64-pc-windows-msvc");
    assert_native_runtime_graph(&tree);
    let packages = packages(&tree);
    for required in ["native-tls", "tokio-native-tls", "schannel", "rcgen", "rsa"] {
        assert!(
            packages.contains(required),
            "Windows lost {required}: {tree}"
        );
    }
    for forbidden in ["openssl", "openssl-sys", "openssl-src"] {
        assert!(
            !packages.contains(forbidden),
            "Windows must use Schannel, found {forbidden}: {tree}"
        );
    }
}

#[test]
fn unix_resolves_rust_runtime_without_retired_engines() {
    let tree = dep_tree("x86_64-unknown-linux-gnu");
    assert_native_runtime_graph(&tree);
    assert!(
        !packages(&tree).contains("openssl-src"),
        "platform OpenSSL must not be vendored: {tree}"
    );
}

#[test]
fn graph_assertions_cannot_confuse_facades_with_runtime_packages() {
    let names =
        packages("susi-vendor-wasmtime v0.1.0\nwasmi v1.0.9\nsusi-vendor-tokio-rustls v0.1.0\n");
    assert!(names.contains("wasmi"));
    assert!(!names.contains("wasmtime"));
    assert!(!names.contains("rustls"));
}
