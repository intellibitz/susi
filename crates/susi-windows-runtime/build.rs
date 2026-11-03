//! Enforce the compiler ABI actually qualified by the native entry fixtures.
use std::{env, io, process::Command};
/// Explicit allow-list of native Windows MSVC targets. x86_64 is qualified;
/// aarch64 is admitted so a native ARM64 host (GitHub `windows-11-arm`, CEO
/// decision 2026-11-03) can run the same fixtures and produce its own
/// evidence. Admission here is not qualification. Cross builds stay refused.
const NATIVE_TARGETS: [&str; 2] = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");
    let target = env::var("TARGET")?;
    if !target.contains("windows") {
        return Ok(());
    }
    if !NATIVE_TARGETS.contains(&target.as_str()) || env::var("HOST")? != target {
        return Err(io::Error::other(
            "Rust entry requires a qualified native x86_64 or aarch64 Windows MSVC toolchain (host equal to target)",
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
    // The compiler's own host line must name the qualified target as well: the
    // same release and commit exist for every rustc host (OSN-22), so release
    // and commit alone do not identify the native compiler that was qualified.
    let host = format!("host: {target}");
    if !identity.lines().any(|line| line == "release: 1.99.0")
        || !identity
            .lines()
            .any(|line| line == "commit-hash: b940084d7eb6a299eb4bfeb8e34901bc051e7ac4")
        || !identity.lines().any(|line| line == host)
    {
        return Err(io::Error::other("unqualified compiler ABI: requalify native Rust entry before changing the compiler pin").into());
    }
    Ok(())
}
