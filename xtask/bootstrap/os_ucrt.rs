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
pub fn generate(output: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(output)?;
    let definition = output.join("susi-os-ucrt.def");
    let library = output.join("susi-os-ucrt.lib");
    fs::write(
        &definition,
        format!("LIBRARY ucrtbase.dll\nEXPORTS\n{SYMBOLS}"),
    )?;
    let result = Command::new(librarian()?)
        .arg("/NOLOGO")
        .arg("/MACHINE:X64")
        .arg(format!("/DEF:{}", definition.display()))
        .arg(format!("/OUT:{}", library.display()))
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "OS-UCRT import metadata generation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(library)
}
pub fn configure() -> io::Result<()> {
    println!("cargo:rerun-if-changed=xtask/bootstrap/os_ucrt.rs");
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
    println!("cargo:rustc-link-arg-bin=susi=/NODEFAULTLIB:vcruntime.lib");
    println!("cargo:rustc-link-arg-bin=susi={}", library.display());
    Ok(())
}
