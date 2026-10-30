#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Build scripts are not production runtime modules.
#![allow(missing_docs)]
//! Links clang's compiler-rt on macOS.
//!
//! ort-sys 2.0.0-rc.15 downloads a prebuilt ONNX Runtime for
//! aarch64-apple-darwin whose CoreML objects use `@available`, which lowers to
//! `___isPlatformVersionAtLeast` in clang's `libclang_rt.osx.a`. clang adds
//! that archive to every C link; rustc links with `-nodefaultlibs`, so the
//! v0.21.24 macOS release leg failed on an undefined symbol (rc.13 did not
//! need it). Naming the archive here puts it on the final link of every
//! binary that embeds the runtime.
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let runtime_dir = [&["xcrun", "clang"][..], &["clang"]]
        .iter()
        .find_map(|argv| {
            let out = Command::new(argv[0])
                .args(&argv[1..])
                .arg("--print-runtime-dir")
                .output()
                .ok()?;
            let dir = String::from_utf8(out.stdout).ok()?.trim().to_string();
            (out.status.success() && Path::new(&dir).join("libclang_rt.osx.a").is_file())
                .then_some(dir)
        });
    match runtime_dir {
        Some(dir) => {
            println!("cargo:rustc-link-search=native={dir}");
            // `-bundle`: a link argument for the final binary, not a copy of
            // compiler-rt inside this rlib.
            println!("cargo:rustc-link-lib=static:-bundle=clang_rt.osx");
        }
        None => println!(
            "cargo:warning=libclang_rt.osx.a not found via `clang --print-runtime-dir`; \
             linking ONNX Runtime may fail on ___isPlatformVersionAtLeast"
        ),
    }
}
