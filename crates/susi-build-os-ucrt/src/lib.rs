//! Import metadata only: Windows supplies implementation from its OS UCRT.
//! This is a capability-qualified OS boundary for an explicit allow-list of
//! native Windows MSVC targets (x64 qualified; ARM64 admitted only to produce
//! its own native evidence), not a promise that private compiler-runtime
//! exports form a stable Microsoft application contract.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

/// Bind a linked PE entry point to its original Rust symbol through the
/// linker map; needs the `link-map` feature (it hashes the map).
#[cfg(feature = "link-map")]
pub mod native_link;
/// Inventory a linked PE image's imports against the OS boundary.
pub mod pe_imports;

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const SYMBOLS: &str = "_CxxThrowException\n__C_specific_handler\n__current_exception\n__current_exception_context\n__CxxFrameHandler3\n__std_type_info_destroy_list\nmemcmp\nmemmove\nmemset\nmemcpy\n";

/// Request content-derived PE debug/timestamp data so independent links of the
/// same inputs are byte-identical; wall time belongs in the build receipt.
pub const REPRO_LINK_ARG: &str = "/Brepro";

/// OS link parameters of one native Windows MSVC target on the allow-list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowsTarget {
    /// Rust target triple; the build host must be the same triple.
    pub triple: &'static str,
    /// Architecture label used in diagnostics.
    label: &'static str,
    /// `lib.exe /MACHINE:` value of the generated import metadata.
    machine: &'static str,
    /// Visual Studio component that supplies this target's MSVC tools.
    component: &'static str,
    /// Librarian paths relative to an MSVC tools root, most preferred first.
    librarians: &'static [&'static str],
    /// OS DLL whose `__chkstk` export satisfies compiler stack probes.
    pub stack_dll: &'static str,
}

/// Qualified native x64 boundary, unchanged from the original x64-only layer.
pub const X64: WindowsTarget = WindowsTarget {
    triple: "x86_64-pc-windows-msvc",
    label: "x64",
    machine: "X64",
    component: "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    librarians: &["bin/Hostx64/x64/lib.exe"],
    // Verified on x64 Windows: kernelbase exports __chkstk (forwarded to ntdll).
    stack_dll: "kernelbase.dll",
};

/// Native ARM64 boundary, admitted only so a native ARM64 host can produce
/// its own evidence. Core CI does not run this target (CEO decision
/// 2026-11-03). It is not qualified until the commercial certified-core
/// pipeline records a passing native run.
pub const ARM64: WindowsTarget = WindowsTarget {
    triple: "aarch64-pc-windows-msvc",
    label: "ARM64",
    machine: "ARM64",
    component: "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
    // Native ARM64-hosted tools first, then x64-hosted ARM64 tools (emulated
    // on an ARM64 host). lib.exe selects the output architecture from
    // /MACHINE, so either writes ARM64 import metadata; an x64-host lib.exe was
    // observed producing ARM64 import libraries. Which directories a native
    // ARM64 image installs is recorded by the commercial certified-core leg.
    librarians: &["bin/Hostarm64/arm64/lib.exe", "bin/Hostx64/arm64/lib.exe"],
    // HYPOTHESIS, not verified: ARM64 kernelbase.dll exports __chkstk as on x64.
    // Locally verified for Rust 1.99.0: the aarch64-pc-windows-msvc
    // compiler_builtins archive defines no __chkstk while the aarch64 std
    // archive references it, so Rust itself does not supply it and an OS (or
    // CRT) definition is required. Windows SDK import libraries list no
    // __chkstk for x64 or ARM64, so they cannot decide this either way. The
    // The commercial certified-core Windows ARM64 leg confirms the export
    // before the fixtures count as evidence, and the 64 KiB stack-probe
    // lifecycle case fails at load (STATUS_ENTRYPOINT_NOT_FOUND) if this
    // choice is wrong. If kernelbase lacks it and ntdll exports it, change
    // this to "ntdll.dll".
    stack_dll: "kernelbase.dll",
};

/// The allow-listed Windows target for `triple`, if any.
pub fn windows_target(triple: &str) -> Option<WindowsTarget> {
    [X64, ARM64]
        .into_iter()
        .find(|target| target.triple == triple)
}

/// The allow-listed Windows target this code was compiled for. Tests run on
/// the host they were built for, so for them this is also the native host.
pub fn native() -> io::Result<WindowsTarget> {
    if cfg!(all(windows, target_arch = "x86_64", target_env = "msvc")) {
        Ok(X64)
    } else if cfg!(all(windows, target_arch = "aarch64", target_env = "msvc")) {
        Ok(ARM64)
    } else {
        Err(io::Error::other(
            "no native Windows OS-UCRT boundary for this target",
        ))
    }
}

fn librarian(target: WindowsTarget) -> io::Result<PathBuf> {
    if let Some(root) = env::var_os("VCToolsInstallDir") {
        for relative in target.librarians {
            let tool = PathBuf::from(&root).join(relative);
            if tool.is_file() {
                return Ok(tool);
            }
        }
    }
    let program_files = env::var_os("ProgramFiles(x86)")
        .ok_or_else(|| io::Error::other("native MSVC installation directory unavailable"))?;
    let locator =
        PathBuf::from(program_files).join("Microsoft Visual Studio/Installer/vswhere.exe");
    for relative in target.librarians {
        let pattern = format!("VC/Tools/MSVC/*/{relative}");
        let output = Command::new(&locator)
            .args([
                "-latest",
                "-products",
                "*",
                "-requires",
                target.component,
                "-find",
                &pattern,
            ])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("MSVC installation discovery failed"));
        }
        let text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
        let mut tools: Vec<_> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .collect();
        tools.sort();
        if let Some(tool) = tools.pop() {
            return Ok(tool);
        }
    }
    Err(io::Error::other(format!(
        "native MSVC {} librarian unavailable; provision the native Rust build toolchain",
        target.label
    )))
}

fn import_library(
    output: &Path,
    name: &str,
    dll: &str,
    symbols: &str,
    target: WindowsTarget,
) -> io::Result<PathBuf> {
    fs::create_dir_all(output)?;
    let definition = output.join(format!("{name}.def"));
    let library = output.join(format!("{name}.lib"));
    fs::write(&definition, format!("LIBRARY {dll}\nEXPORTS\n{symbols}"))?;
    let result = Command::new(librarian(target)?)
        .arg("/NOLOGO")
        .arg(format!("/MACHINE:{}", target.machine))
        .arg(format!("/DEF:{}", definition.display()))
        .arg(format!("/OUT:{}", library.display()))
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "OS import metadata generation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(library)
}

/// OS-UCRT import metadata for the native target this code was compiled for.
pub fn generate(output: &Path) -> io::Result<PathBuf> {
    generate_for(output, native()?)
}

/// OS-UCRT import metadata for one allow-listed Windows target.
pub fn generate_for(output: &Path, target: WindowsTarget) -> io::Result<PathBuf> {
    import_library(output, "susi-os-ucrt", "ucrtbase.dll", SYMBOLS, target)
}

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

/// Resolve the single binary target name from a `Cargo.toml` manifest text,
/// falling back to `pkg_name` when no explicit `[[bin]]` section is present.
pub fn single_bin_name_from_manifest(manifest_toml: &str, pkg_name: &str) -> String {
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

/// Legacy single-bin entry point (`configure_for(&["susi"])`).
pub fn configure() -> io::Result<()> {
    configure_for(&["susi"])
}

/// Configure link arguments across all binary targets of the calling package
/// (`cargo:rustc-link-arg-bins=`). Used by single-binary packages so renaming
/// the binary in `Cargo.toml` requires no `build.rs` edit.
pub fn configure_all_bins() -> io::Result<()> {
    for name in ["HOST", "TARGET", "VCToolsInstallDir", "ProgramFiles(x86)"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
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
    if !target.contains("windows") {
        return Ok(());
    }
    let host = env::var("HOST").map_err(io::Error::other)?;
    let Some(windows) = windows_target(&target).filter(|_| host == target) else {
        return Err(io::Error::other("OS-UCRT boundary is qualified only for native x64 Windows MSVC (native ARM64 is admitted only to produce its own evidence); other Windows targets and cross builds require their own ABI acceptance evidence"));
    };
    let output = PathBuf::from(
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing Cargo OUT_DIR"))?,
    );
    let library = generate_for(&output, windows)?;
    let stack = import_library(
        &output,
        "susi-os-stack",
        windows.stack_dll,
        "__chkstk\n",
        windows,
    )?;
    for argument in [
        REPRO_LINK_ARG,
        "/ENTRY:susi_pe_entry",
        "/INCLUDE:_tls_used",
        "/NODEFAULTLIB:msvcrt.lib",
        "/NODEFAULTLIB:vcruntime.lib",
        "/MERGE:.CRT=.rdata",
        "ucrt.lib",
    ] {
        println!("cargo:rustc-link-arg-bins={argument}");
    }
    println!("cargo:rustc-link-arg-bins={}", library.display());
    println!("cargo:rustc-link-arg-bins={}", stack.display());
    println!("cargo:rustc-link-arg-bins=/MAP");
    Ok(())
}

/// Configure link arguments for an explicit list of binary targets (`cargo:rustc-link-arg-bin=<bin>=`).
pub fn configure_for(binaries: &[&str]) -> io::Result<()> {
    for name in ["HOST", "TARGET", "VCToolsInstallDir", "ProgramFiles(x86)"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
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
    if !target.contains("windows") {
        return Ok(());
    }
    let host = env::var("HOST").map_err(io::Error::other)?;
    let Some(windows) = windows_target(&target).filter(|_| host == target) else {
        return Err(io::Error::other("OS-UCRT boundary is qualified only for native x64 Windows MSVC (native ARM64 is admitted only to produce its own evidence); other Windows targets and cross builds require their own ABI acceptance evidence"));
    };
    let output = PathBuf::from(
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing Cargo OUT_DIR"))?,
    );
    let library = generate_for(&output, windows)?;
    let stack = import_library(
        &output,
        "susi-os-stack",
        windows.stack_dll,
        "__chkstk\n",
        windows,
    )?;
    for binary in binaries {
        validate_binary_name(binary)?;
        for argument in [
            REPRO_LINK_ARG,
            "/ENTRY:susi_pe_entry",
            "/INCLUDE:_tls_used",
            "/NODEFAULTLIB:msvcrt.lib",
            "/NODEFAULTLIB:vcruntime.lib",
            "/MERGE:.CRT=.rdata",
            "ucrt.lib",
        ] {
            println!("cargo:rustc-link-arg-bin={binary}={argument}");
        }
        println!("cargo:rustc-link-arg-bin={binary}={}", library.display());
        println!("cargo:rustc-link-arg-bin={binary}={}", stack.display());
        println!("cargo:rustc-link-arg-bin={binary}=/MAP");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_target_lookup_recognizes_only_allow_listed_triples() {
        assert_eq!(windows_target("x86_64-pc-windows-msvc"), Some(X64));
        assert_eq!(windows_target("aarch64-pc-windows-msvc"), Some(ARM64));
        assert_eq!(windows_target("i686-pc-windows-msvc"), None);
        assert_eq!(windows_target("x86_64-pc-windows-gnu"), None);
        assert_eq!(windows_target("x86_64-unknown-linux-gnu"), None);
    }

    #[test]
    fn single_bin_name_resolves_explicit_bin_or_falls_back_to_package_name() {
        let implicit = "[package]\nname = \"susi\"\nversion = \"0.21.48\"\n";
        assert_eq!(single_bin_name_from_manifest(implicit, "susi"), "susi");

        let renamed =
            "[package]\nname = \"susi\"\n\n[[bin]]\nname = \"susid\"\npath = \"src/main.rs\"\n";
        assert_eq!(single_bin_name_from_manifest(renamed, "susi"), "susid");
    }

    #[test]
    fn validate_binary_name_accepts_alphanumeric_and_dash_only() {
        assert!(validate_binary_name("susi").is_ok());
        assert!(validate_binary_name("susid").is_ok());
        assert!(validate_binary_name("local-release").is_ok());
        assert!(validate_binary_name("").is_err());
        assert!(validate_binary_name("susi_dev").is_err());
        assert!(validate_binary_name("../susi").is_err());
    }

    #[test]
    fn linux_profile_dir_requires_cargo_build_out_layout() {
        let valid = Path::new("/tmp/target/x86_64-unknown-linux-gnu/release/build/susi-123/out");
        assert_eq!(
            linux_profile_dir(valid).unwrap(),
            Path::new("/tmp/target/x86_64-unknown-linux-gnu/release")
        );

        let invalid = Path::new("/tmp/target/release/deps");
        assert!(linux_profile_dir(invalid).is_err());
    }
}
