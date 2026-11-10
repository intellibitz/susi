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
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "LD_AUDIT",
        "DYLD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
    ] {
        command.env_remove(key);
    }
}
#[allow(dead_code)]
pub(crate) fn configure_system_openssl(
    command: &mut std::process::Command,
    target: &str,
) -> Result<(), String> {
    let libdir = system_openssl_libdir(target, |path| path.canonicalize())?;
    let keys: Vec<_> = std::env::vars_os()
        .map(|(key, _)| key)
        .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
        .collect();
    for key in keys {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.contains("OPENSSL")
            || name.contains("PKG_CONFIG")
            || matches!(
                name.as_str(),
                "LIBRARY_PATH" | "LD_LIBRARY_PATH" | "LD_PRELOAD" | "LD_AUDIT"
            )
        {
            command.env_remove(key);
        }
    }
    command
        .env("OPENSSL_STATIC", "0")
        .env("OPENSSL_NO_VENDOR", "1")
        .env("OPENSSL_NO_PKG_CONFIG", "1")
        .env("OPENSSL_LIB_DIR", libdir)
        .env("OPENSSL_INCLUDE_DIR", "/usr/include")
        .env("OPENSSL_LIBS", "ssl:crypto");
    Ok(())
}

fn system_openssl_libdir(
    target: &str,
    mut resolve: impl FnMut(&Path) -> io::Result<PathBuf>,
) -> Result<PathBuf, String> {
    let (multiarch, native_arch) = match target {
        "x86_64-unknown-linux-gnu" => ("x86_64-linux-gnu", cfg!(target_arch = "x86_64")),
        "aarch64-unknown-linux-gnu" => ("aarch64-linux-gnu", cfg!(target_arch = "aarch64")),
        _ => return Err("system OpenSSL configuration requires reviewed Linux target".into()),
    };
    // Prefer the existing Debian layout. Arch-family distributions install
    // their native development libraries directly in /usr/lib. Never consult
    // caller paths, and do not use native libraries for a different target.
    let directories = [
        Path::new("/usr/lib").join(multiarch),
        PathBuf::from("/usr/lib"),
    ];
    for libdir in directories
        .into_iter()
        .take(if native_arch { 2 } else { 1 })
    {
        let mut missing = false;
        for name in ["libssl.so", "libcrypto.so"] {
            match resolve(&libdir.join(name)) {
                Ok(resolved) => {
                    if !resolved.starts_with("/usr/lib") && !resolved.starts_with("/lib") {
                        return Err("OpenSSL library resolves outside OS directories".into());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => missing = true,
                Err(error) => {
                    return Err(format!("cannot resolve system OpenSSL library: {error}"))
                }
            }
        }
        if !missing {
            return Ok(libdir);
        }
    }
    Err("system OpenSSL development libraries missing".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openssl_prefers_complete_multiarch_directory() {
        let selected =
            system_openssl_libdir("x86_64-unknown-linux-gnu", |path| Ok(path.into())).unwrap();
        assert_eq!(selected, Path::new("/usr/lib").join("x86_64-linux-gnu"));
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn openssl_accepts_complete_native_flat_directory() {
        let selected = system_openssl_libdir("x86_64-unknown-linux-gnu", |path| {
            if path.parent() == Some(Path::new("/usr/lib")) {
                Ok(path.into())
            } else {
                Err(io::ErrorKind::NotFound.into())
            }
        })
        .unwrap();
        assert_eq!(selected, Path::new("/usr/lib"));
    }

    #[test]
    fn openssl_requires_both_libraries_in_one_directory() {
        let result = system_openssl_libdir("x86_64-unknown-linux-gnu", |path| {
            let multiarch = path.parent() == Some(&Path::new("/usr/lib").join("x86_64-linux-gnu"));
            if multiarch == (path.file_name() == Some(OsStr::new("libssl.so"))) {
                Ok(path.into())
            } else {
                Err(io::ErrorKind::NotFound.into())
            }
        });
        assert_eq!(
            result.unwrap_err(),
            "system OpenSSL development libraries missing"
        );
    }

    #[test]
    fn openssl_rejects_escaped_library_without_falling_back() {
        for outside in [
            "/tmp/libssl.so",
            "/usr/library/libssl.so",
            "/library/libssl.so",
        ] {
            let mut calls = 0;
            let result = system_openssl_libdir("x86_64-unknown-linux-gnu", |_| {
                calls += 1;
                Ok(PathBuf::from(outside))
            });
            assert_eq!(
                result.unwrap_err(),
                "OpenSSL library resolves outside OS directories"
            );
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn openssl_rejects_unreadable_library_without_falling_back() {
        let mut calls = 0;
        let result = system_openssl_libdir("x86_64-unknown-linux-gnu", |_| {
            calls += 1;
            Err(io::ErrorKind::PermissionDenied.into())
        });
        assert!(result
            .unwrap_err()
            .starts_with("cannot resolve system OpenSSL library:"));
        assert_eq!(calls, 1);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn openssl_does_not_use_flat_native_libraries_for_foreign_architecture() {
        let mut paths = Vec::new();
        let result = system_openssl_libdir("aarch64-unknown-linux-gnu", |path| {
            paths.push(path.to_owned());
            Err(io::ErrorKind::NotFound.into())
        });
        assert!(result.is_err());
        assert_eq!(paths.len(), 2);
        assert!(paths
            .iter()
            .all(|path| path.parent() == Some(&Path::new("/usr/lib").join("aarch64-linux-gnu"))));
    }

    #[test]
    fn openssl_rejects_unreviewed_target_before_discovery() {
        let mut inspected = false;
        let result = system_openssl_libdir("x86_64-pc-windows-msvc", |_| {
            inspected = true;
            Err(io::ErrorKind::NotFound.into())
        });
        assert_eq!(
            result.unwrap_err(),
            "system OpenSSL configuration requires reviewed Linux target"
        );
        assert!(!inspected);
    }

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
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
            "LD_AUDIT",
        ] {
            assert!(command
                .get_envs()
                .any(|(name, value)| name == key && value.is_none()));
        }
    }
}
