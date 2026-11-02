//! Explicit compiler environment for local acceptance. Empty encoded flags
//! take precedence over Cargo config rustflags; inherited flags cannot smuggle
//! static foreign runtime objects into an apparently OS-only PE image.
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

const RUST_COMMIT: &str = "b940084d7eb6a299eb4bfeb8e34901bc051e7ac4";
const CARGO_COMMIT: &str = "5f94df4789f005f9a352888e8355ffc645b7ed0e";

fn tool(bin: &Path, name: &str) -> PathBuf {
    bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

fn discover() -> io::Result<PathBuf> {
    // The installed compiler/toolchain is a platform boundary. Consult the
    // provisioned compiler once, then select the real tools in that sysroot.
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()?;
    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !output.status.success() || !root.is_absolute() {
        return Err(io::Error::other(
            "cannot resolve an absolute installed Rust sysroot",
        ));
    }
    let bin = root.join("bin").canonicalize()?;
    let mut compiler_host = None;
    for (name, commit) in [
        ("rustc", RUST_COMMIT),
        ("rustdoc", RUST_COMMIT),
        ("cargo", CARGO_COMMIT),
    ] {
        let output = Command::new(tool(&bin, name)).arg("-Vv").output()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let host = text.lines().find_map(|line| line.strip_prefix("host: "));
        if !output.status.success()
            || !text.lines().any(|line| line == "release: 1.99.0")
            || !text
                .lines()
                .any(|line| line.strip_prefix("commit-hash: ") == Some(commit))
            || host.is_none_or(str::is_empty)
            || compiler_host
                .as_deref()
                .is_some_and(|expected| Some(expected) != host)
            || (cfg!(windows) && !host.is_some_and(|value| value.ends_with("-pc-windows-msvc")))
        {
            return Err(io::Error::other(format!(
                "unqualified installed {name} in {}",
                bin.display()
            )));
        }
        compiler_host = host.map(str::to_owned);
    }
    Ok(bin)
}

pub(crate) fn apply_checked(command: &mut Command) -> io::Result<()> {
    let explicit: Vec<_> = command.get_envs().map(|(key, _)| key.to_owned()).collect();
    apply_names(
        command,
        std::env::vars_os().map(|(key, _)| key).chain(explicit),
    );
    let program = Path::new(command.get_program());
    if !program.file_stem().is_some_and(|name| {
        ["cargo", "rustc", "rustdoc"]
            .iter()
            .any(|tool| name == *tool)
    }) {
        return Ok(());
    }
    static BIN: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let bin = BIN
        .get_or_init(|| discover().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))?;
    if program.components().count() != 1 && program.canonicalize()?.parent() != Some(bin.as_path())
    {
        return Err(io::Error::other(
            "requested Rust executable is outside the selected toolchain",
        ));
    }
    // Rust's native process launcher searches an explicit child PATH first.
    // Preserve arguments, streams, cwd and other caller-specified environment.
    let inherited = std::env::var_os("PATH");
    let previous = command
        .get_envs()
        .find(|(key, _)| key.eq_ignore_ascii_case(OsStr::new("PATH")))
        .map(|(_, value)| value)
        .unwrap_or(inherited.as_deref());
    let path = std::env::join_paths(
        std::iter::once(bin.clone()).chain(
            previous
                .into_iter()
                .flat_map(std::env::split_paths)
                .filter(|path| path != bin),
        ),
    )
    .map_err(io::Error::other)?;
    command
        .env("PATH", path)
        .env("RUSTC", tool(bin, "rustc"))
        .env("RUSTDOC", tool(bin, "rustdoc"));
    Ok(())
}

// Older sysroot callers configure a Command infallibly before spawning it.
// Poison the program itself on discovery failure. No OS accepts a NUL in an
// executable name, so a later env/current_dir update cannot undo the refusal.
pub(crate) fn apply(command: &mut Command) {
    if let Err(error) = apply_checked(command) {
        eprintln!("local compiler binding refused: {error}");
        *command = Command::new("invalid Rust toolchain\0");
    }
}
fn apply_names(command: &mut Command, names: impl Iterator<Item = OsString>) {
    for key in names {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("CARGO_TARGET_")
            && (name.ends_with("_RUSTFLAGS")
                || name.ends_with("_LINKER")
                || name.ends_with("_RUNNER"))
        {
            command.env_remove(key);
        }
    }
    for key in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "RUSTDOCFLAGS",
        "CARGO_ENCODED_RUSTDOCFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env(key, "");
    }
    // Compiler selection belongs to the installed native toolchain, not an
    // inherited per-process replacement executable or bootstrap permission.
    for key in [
        "RUSTC",
        "RUSTDOC",
        "RUSTC_BOOTSTRAP",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env_remove(key);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_static_crt_flags_and_target_overrides_cannot_reach_cargo() {
        let mut command = Command::new("cargo");
        command
            .env("RUSTFLAGS", "-Ctarget-feature=+crt-static")
            .env("CARGO_ENCODED_RUSTFLAGS", "-Ctarget-feature=+crt-static")
            .env(
                "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
                "foreign-linker",
            );
        apply_names(
            &mut command,
            [
                "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
                "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS",
            ]
            .into_iter()
            .map(OsString::from),
        );
        for key in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTDOCFLAGS"] {
            assert!(command
                .get_envs()
                .any(|(name, value)| name == key && value == Some(std::ffi::OsStr::new(""))));
        }
        for key in [
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS",
            "RUSTC_BOOTSTRAP",
        ] {
            assert!(command
                .get_envs()
                .any(|(name, value)| name == key && value.is_none()));
        }
    }
}
