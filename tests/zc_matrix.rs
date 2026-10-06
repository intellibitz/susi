#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The Zero-Config Matrix windows leg failed every run on main: wasmer's
//! `default` feature is `sys-default` (sys+wat+cranelift+demangle), so the
//! windows dependency listing `singlepass` without `default-features =
//! false` still compiled `wasmer-compiler-cranelift` — a `compile_error!`
//! on Windows. These pin the resolved feature set per target without
//! needing a Windows toolchain: `cargo tree --target` resolves cfg-gated
//! deps and features offline.

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
fn windows_never_resolves_the_cranelift_backend() {
    let tree = dep_tree("x86_64-pc-windows-msvc");
    assert!(
        !tree.contains("wasmer-compiler-cranelift"),
        "the windows build graph must not contain wasmer-compiler-cranelift:\n{tree}"
    );
    // The windows wasm backend is Singlepass; it must still be there.
    assert!(
        tree.contains("wasmer-compiler-singlepass"),
        "windows lost its wasm backend entirely:\n{tree}"
    );
}

#[test]
fn unix_keeps_cranelift() {
    let tree = dep_tree("x86_64-unknown-linux-gnu");
    assert!(
        tree.contains("wasmer-compiler-cranelift"),
        "the linux backend regressed:\n{tree}"
    );
}
