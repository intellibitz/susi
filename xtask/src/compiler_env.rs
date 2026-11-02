//! Explicit compiler environment for local acceptance. Empty encoded flags
//! take precedence over Cargo config rustflags; inherited flags cannot smuggle
//! static foreign runtime objects into an apparently OS-only PE image.
use std::{ffi::OsString, process::Command};
pub(crate) fn apply(command: &mut Command) {
    apply_names(command, std::env::vars_os().map(|(key, _)| key));
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
pub(crate) fn configure_system_openssl(
    command: &mut std::process::Command,
    target: &str,
) -> Result<(), String> {
    let multiarch = match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        _ => return Err("system OpenSSL configuration requires reviewed Linux target".into()),
    };
    let libdir = std::path::Path::new("/usr/lib").join(multiarch);
    for name in ["libssl.so", "libcrypto.so"] {
        let resolved = libdir
            .join(name)
            .canonicalize()
            .map_err(|_| "system OpenSSL development libraries missing")?;
        if !resolved.starts_with("/usr/lib") && !resolved.starts_with("/lib") {
            return Err("OpenSSL library resolves outside OS directories".into());
        }
    }
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
