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

fn dep_tree_with_features(target: &str, all_features: bool) -> String {
    let mut command = Command::new(env!("CARGO"));
    command.arg("tree");
    if all_features {
        command.args(["--all-features", "--workspace"]);
    }
    let out = command
        .args([
            "--target",
            target,
            "-e",
            "normal,build",
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

fn dep_tree(target: &str) -> String {
    dep_tree_with_features(target, false)
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

#[test]
fn all_supported_features_exclude_retired_accelerator_dependencies() {
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        let tree = dep_tree_with_features(target, true);
        assert_native_runtime_graph(&tree);
        let names = packages(&tree);
        for retired in [
            "intel-mkl-src",
            "intel-mkl-tool",
            "cudarc",
            "candle-kernels",
            "libloading",
            "option-ext",
            "ocipkg",
        ] {
            assert!(
                !names.contains(retired),
                "{target} selected retired {retired}: {tree}"
            );
        }
    }
}

#[test]
fn retired_feature_requests_fail_explicitly_before_building() {
    for package in [
        "susi",
        "susi-gemi",
        "susi-gemi-models",
        "susi-vendor-candle",
    ] {
        for feature in ["cuda", "mkl"] {
            let output = Command::new(env!("CARGO"))
                .args([
                    "tree",
                    "--offline",
                    "--locked",
                    "-p",
                    package,
                    "--features",
                    feature,
                ])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{package}/{feature} must not silently build CPU"
            );
            assert!(
                error.contains(feature)
                    && (error.contains("does not contain") || error.contains("does not have")),
                "unexpected rejection: {error}"
            );
        }
    }
}
