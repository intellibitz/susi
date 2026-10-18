#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The Zero-Config Matrix windows leg failed every run on main: every native
//! wasmer compiler backend except V8 carries a `compile_error!` under
//! `cfg(windows)` at 7.4.2, and singlepass is BUSL-1.1 besides. The vendored
//! wasm engine is now wasmtime — Apache-2.0, first-class on windows-msvc —
//! so the graph assertions pin that the vendored runtime is wasmtime on
//! every target and that no wasmer crate can creep back into the build.
//! `cargo tree --target` resolves cfg-gated deps and features offline, so
//! these run without the target toolchain installed.

use std::process::Command;

fn dep_tree(target: &str) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree", "--target", target, "-e", "normal", "--prefix", "none", "--locked", "-p",
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

#[test]
fn windows_resolves_wasmtime_not_wasmer() {
    let tree = dep_tree("x86_64-pc-windows-msvc");
    assert!(
        tree.contains("wasmtime"),
        "windows lost its wasm backend entirely:\n{tree}"
    );
    assert!(
        !tree.contains("wasmer"),
        "the windows build graph must not contain wasmer (no compliant \
         windows backend exists at 7.4.2; singlepass is BUSL-1.1):\n{tree}"
    );
}

#[test]
fn unix_resolves_wasmtime_not_wasmer() {
    let tree = dep_tree("x86_64-unknown-linux-gnu");
    assert!(
        tree.contains("wasmtime"),
        "the linux wasm backend regressed:\n{tree}"
    );
    assert!(
        !tree.contains("wasmer"),
        "the linux build graph must not contain wasmer:\n{tree}"
    );
}
