//! Linux-only linker map configuration for core build scripts.
//! Core ships for Linux only (Mandate 84); Windows PE/UCRT lives in
//! Susi Commercial Apps (`susi-build-os-ucrt`).
#![allow(dead_code)]

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn validate_binary_name(binary: &str) -> io::Result<()> {
    if binary.is_empty()
        || !binary
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(io::Error::other("invalid native binary name"));
    }
    Ok(())
}

fn linux_profile_dir(output: &Path) -> io::Result<&Path> {
    let profile = output
        .ancestors()
        .nth(3)
        .ok_or_else(|| io::Error::other("invalid Cargo output layout"))?;
    if output.file_name().is_none_or(|name| name != "out")
        || output
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_none_or(|name| name != "build")
    {
        return Err(io::Error::other("unexpected Cargo build output layout"));
    }
    Ok(profile)
}

fn single_bin_name_from_manifest(manifest_toml: &str, pkg_name: &str) -> String {
    let mut in_bin = false;
    for line in manifest_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_bin = trimmed == "[[bin]]";
            continue;
        }
        if in_bin {
            if let Some(rest) = trimmed.strip_prefix("name") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    if let Some(name) = rest.split('"').nth(1) {
                        if !name.is_empty() {
                            return name.to_string();
                        }
                    }
                }
            }
        }
    }
    pkg_name.to_string()
}

fn current_package_single_bin_name() -> String {
    let pkg_name = env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "susi".to_string());
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let manifest_toml = fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap_or_default();
    single_bin_name_from_manifest(&manifest_toml, &pkg_name)
}

pub fn configure_all_bins() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");
    let target = env::var("TARGET").map_err(io::Error::other)?;
    if matches!(
        target.as_str(),
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    ) {
        let output = PathBuf::from(
            env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing Cargo OUT_DIR"))?,
        );
        let profile = linux_profile_dir(&output)?;
        let binary = current_package_single_bin_name();
        validate_binary_name(&binary)?;
        let map = profile.join(format!("{binary}.map"));
        let map = map
            .to_str()
            .ok_or_else(|| io::Error::other("nonUTF8 map path"))?;
        if map.contains([',', '\n', '\r']) {
            return Err(io::Error::other("invalid linker map path"));
        }
        println!("cargo:rustc-link-arg-bins=-fuse-ld=lld");
        println!("cargo:rustc-link-arg-bins=-Wl,-Map={map}");
        return Ok(());
    }
    Ok(())
}

pub fn configure_for(binaries: &[&str]) -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=HOST");
    println!("cargo:rerun-if-env-changed=TARGET");
    let target = env::var("TARGET").map_err(io::Error::other)?;
    if matches!(
        target.as_str(),
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    ) {
        let output = PathBuf::from(
            env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing Cargo OUT_DIR"))?,
        );
        let profile = linux_profile_dir(&output)?;
        for binary in binaries {
            validate_binary_name(binary)?;
            let map = profile.join(format!("{binary}.map"));
            let map = map
                .to_str()
                .ok_or_else(|| io::Error::other("nonUTF8 map path"))?;
            if map.contains([',', '\n', '\r']) {
                return Err(io::Error::other("invalid linker map path"));
            }
            println!("cargo:rustc-link-arg-bin={binary}=-fuse-ld=lld");
            println!("cargo:rustc-link-arg-bin={binary}=-Wl,-Map={map}");
        }
        return Ok(());
    }
    Ok(())
}
