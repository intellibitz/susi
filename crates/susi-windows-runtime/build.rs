//! Enforce the compiler ABI actually qualified by the native entry fixtures.
use std::{env, io, process::Command};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");
    let target = env::var("TARGET")?;
    if !target.contains("windows") {
        return Ok(());
    }
    if target != "x86_64-pc-windows-msvc" || env::var("HOST")? != target {
        return Err(io::Error::other(
            "Rust entry requires the qualified native x64 Windows MSVC toolchain",
        )
        .into());
    }
    let compiler = env::var_os("RUSTC")
        .ok_or_else(|| io::Error::other("Cargo compiler identity unavailable"))?;
    let output = Command::new(compiler)
        .arg("--version")
        .arg("--verbose")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("compiler identity query failed").into());
    }
    let identity = String::from_utf8(output.stdout)?;
    if !identity.lines().any(|line| line == "release: 1.99.0")
        || !identity
            .lines()
            .any(|line| line == "commit-hash: b940084d7eb6a299eb4bfeb8e34901bc051e7ac4")
    {
        return Err(io::Error::other("unqualified compiler ABI: requalify native Rust entry before changing the compiler pin").into());
    }
    Ok(())
}
