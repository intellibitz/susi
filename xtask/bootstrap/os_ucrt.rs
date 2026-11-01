//! Import metadata only: Windows supplies implementation from its OS UCRT.
//! This is a capability-qualified x64 OS boundary, not a promise that private
//! compiler-runtime exports form a stable Microsoft application contract.
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};
const SYMBOLS:&str="_CxxThrowException\n__C_specific_handler\n__current_exception\n__current_exception_context\n__CxxFrameHandler3\n__std_type_info_destroy_list\nmemcmp\nmemmove\nmemset\nmemcpy\n";
fn librarian() -> io::Result<PathBuf> {
    if let Some(root) = env::var_os("VCToolsInstallDir") {
        let tool = PathBuf::from(root).join("bin/Hostx64/x64/lib.exe");
        if tool.is_file() {
            return Ok(tool);
        }
    }
    let program_files = env::var_os("ProgramFiles(x86)")
        .ok_or_else(|| io::Error::other("native MSVC installation directory unavailable"))?;
    let locator =
        PathBuf::from(program_files).join("Microsoft Visual Studio/Installer/vswhere.exe");
    let output = Command::new(locator)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-find",
            "VC/Tools/MSVC/*/bin/Hostx64/x64/lib.exe",
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
    tools.pop().ok_or_else(|| {
        io::Error::other(
            "native MSVC x64 librarian unavailable; provision the native Rust build toolchain",
        )
    })
}
fn import_library(output: &Path, name: &str, dll: &str, symbols: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(output)?;
    let definition = output.join(format!("{name}.def"));
    let library = output.join(format!("{name}.lib"));
    fs::write(&definition, format!("LIBRARY {dll}\nEXPORTS\n{symbols}"))?;
    let result = Command::new(librarian()?)
        .arg("/NOLOGO")
        .arg("/MACHINE:X64")
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
pub fn generate(output: &Path) -> io::Result<PathBuf> {
    import_library(output, "susi-os-ucrt", "ucrtbase.dll", SYMBOLS)
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
    if !target.contains("windows") {
        return Ok(());
    }
    let host = env::var("HOST").map_err(io::Error::other)?;
    if target != "x86_64-pc-windows-msvc" || host != target {
        return Err(io::Error::other("OS-UCRT boundary is qualified only for native x64 Windows MSVC; other Windows targets require their own ABI acceptance evidence"));
    }
    let output = PathBuf::from(
        env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("missing Cargo OUT_DIR"))?,
    );
    let library = generate(&output)?;
    let stack = import_library(&output, "susi-os-stack", "kernelbase.dll", "__chkstk\n")?;
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
