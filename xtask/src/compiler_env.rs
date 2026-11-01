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
