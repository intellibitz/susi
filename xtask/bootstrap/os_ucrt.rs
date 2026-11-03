//! Import metadata only: Windows supplies implementation from its OS UCRT.
//! This is a capability-qualified OS boundary for an explicit allow-list of
//! native Windows MSVC targets (x64 qualified; ARM64 admitted only to produce
//! its own native evidence), not a promise that private compiler-runtime
//! exports form a stable Microsoft application contract.
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};
const SYMBOLS:&str="_CxxThrowException\n__C_specific_handler\n__current_exception\n__current_exception_context\n__CxxFrameHandler3\n__std_type_info_destroy_list\nmemcmp\nmemmove\nmemset\nmemcpy\n";
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
/// Native ARM64 boundary, admitted only so a native ARM64 host (GitHub
/// `windows-11-arm`, CEO decision 2026-11-03) can produce its own evidence.
/// It is not qualified until that evidence passes.
pub const ARM64: WindowsTarget = WindowsTarget {
    triple: "aarch64-pc-windows-msvc",
    label: "ARM64",
    machine: "ARM64",
    component: "Microsoft.VisualStudio.Component.VC.Tools.ARM64",
    // Native ARM64-hosted tools first, then x64-hosted ARM64 tools (emulated
    // on an ARM64 host). lib.exe selects the output architecture from
    // /MACHINE, so either writes ARM64 import metadata; an x64-host lib.exe was
    // observed producing ARM64 import libraries. Which directories the runner
    // image installs is recorded by the windows-arm64-probe workflow.
    librarians: &["bin/Hostarm64/arm64/lib.exe", "bin/Hostx64/arm64/lib.exe"],
    // HYPOTHESIS, not verified: ARM64 kernelbase.dll exports __chkstk as on x64.
    // Locally verified for Rust 1.99.0: the aarch64-pc-windows-msvc
    // compiler_builtins archive defines no __chkstk while the aarch64 std
    // archive references it, so Rust itself does not supply it and an OS (or
    // CRT) definition is required. Windows SDK import libraries list no
    // __chkstk for x64 or ARM64, so they cannot decide this either way. The
    // windows-arm64-probe workflow dumps the OS kernelbase.dll and ntdll.dll
    // exports, the Windows ARM64 CI job asserts the export before running the
    // fixtures, and the 64 KiB stack-probe lifecycle case fails at load
    // (STATUS_ENTRYPOINT_NOT_FOUND) if this choice is wrong. If kernelbase
    // lacks it and ntdll exports it, change this to "ntdll.dll".
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn generate(output: &Path) -> io::Result<PathBuf> {
    generate_for(output, native()?)
}
/// OS-UCRT import metadata for one allow-listed Windows target.
pub fn generate_for(output: &Path, target: WindowsTarget) -> io::Result<PathBuf> {
    import_library(output, "susi-os-ucrt", "ucrtbase.dll", SYMBOLS, target)
}
pub fn configure() -> io::Result<()> {
    configure_for(&["susi"])
}
pub fn configure_for(binaries: &[&str]) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", file!());
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
        for binary in binaries {
            if binary.is_empty()
                || !binary
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(io::Error::other("invalid native binary name"));
            }
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
        if binary.is_empty()
            || !binary
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(io::Error::other("invalid native binary name"));
        }
        for argument in [
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
