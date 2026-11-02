#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Resolve the actual target dependency graph, not historical facade names.
//! Wasmi supplies Rust-only WebAssembly execution. Windows TLS uses Schannel,
//! Apple uses Security.framework, and Linux uses platform OpenSSL. These are
//! explicit OS boundaries, not claims of pure Rust crypto implementations.
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
                    | "rsa"
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
    for required in [
        "native-tls",
        "tokio-native-tls",
        "schannel",
        "windows-sys",
        "rcgen",
        "pkcs1",
        "pkcs8",
    ] {
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
fn linux_resolves_rust_runtime_and_platform_openssl() {
    let tree = dep_tree("x86_64-unknown-linux-gnu");
    assert_native_runtime_graph(&tree);
    let packages = packages(&tree);
    for required in [
        "native-tls",
        "tokio-native-tls",
        "openssl",
        "openssl-sys",
        "rcgen",
        "pkcs1",
        "pkcs8",
    ] {
        assert!(packages.contains(required), "Linux lost {required}: {tree}");
    }
    for forbidden in ["openssl-src", "schannel", "security-framework"] {
        assert!(
            !packages.contains(forbidden),
            "Linux must use installed platform OpenSSL, found {forbidden}: {tree}"
        );
    }
}

#[test]
fn apple_resolves_rust_runtime_and_security_framework() {
    let tree = dep_tree("aarch64-apple-darwin");
    assert_native_runtime_graph(&tree);
    let packages = packages(&tree);
    for required in [
        "native-tls",
        "tokio-native-tls",
        "security-framework",
        "security-framework-sys",
        "core-foundation",
        "rcgen",
        "pkcs1",
        "pkcs8",
    ] {
        assert!(packages.contains(required), "Apple lost {required}: {tree}");
    }
    for forbidden in ["openssl", "openssl-sys", "openssl-src", "schannel"] {
        assert!(
            !packages.contains(forbidden),
            "Apple must use Security.framework, found {forbidden}: {tree}"
        );
    }
}

#[test]
fn graph_assertions_cannot_confuse_facades_with_runtime_packages() {
    let names =
        packages("susi-vendor-wasmtime v0.1.0\nwasmi v1.0.9\nsusi-vendor-tokio-rustls v0.1.0\n");
    assert!(names.contains("wasmi"));
    assert!(!names.contains("wasmtime"));
    assert!(!names.contains("rustls"));
}
