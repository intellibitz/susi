//! Installed runtime host discovery and Linux GPG multicall fallback.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Names the plug/runtime host explicitly, ahead of `PATH`.
pub const RUNTIME_HOST_ENV: &str = "SUSI_RUNTIME_HOST";
/// Compatibility alias for [`RUNTIME_HOST_ENV`].
pub const COMMERCIAL_BIN_ENV: &str = "SUSI_COMMERCIAL_BIN";

const RUNTIME_HOST_BINS: [&str; 2] = ["susi-runtime-host", "susi-commercial"];

#[doc(hidden)]
pub fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[doc(hidden)]
pub fn home_dir() -> Option<PathBuf> {
    non_empty_env("HOME").map(PathBuf::from)
}

/// Search each PATH directory in order before inspecting installation-specific fallbacks.
#[doc(hidden)]
pub fn find_program(
    path: Option<&std::ffi::OsStr>,
    names: &[&str],
    fallbacks: impl IntoIterator<Item = PathBuf>,
    accepts: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let on_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)));
    on_path
        .chain(fallbacks)
        .find(|candidate| accepts(candidate))
}

#[doc(hidden)]
pub fn is_susi_wrapper_binary(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let name = text.rsplit(['/', '\\']).next().unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    matches!(
        stem,
        "susi" | "susi-gpg" | "susi-git-gpg" | "susi-gpg-program" | "git-gpg-agent"
    )
}

#[doc(hidden)]
pub fn resolve_real_gpg_binary() -> Option<String> {
    if let Some(raw) = non_empty_env("SUSI_REAL_GPG") {
        let p = Path::new(&raw);
        if !is_susi_wrapper_binary(p) && (p.is_file() || !raw.contains(['/', '\\'])) {
            return Some(raw);
        }
    }
    let extras = ["/usr/bin/gpg", "/usr/bin/gpg2", "/usr/local/bin/gpg"].map(PathBuf::from);
    find_program(
        std::env::var_os("PATH").as_deref(),
        &["gpg", "gpg2"],
        extras,
        |candidate| candidate.is_file() && !is_susi_wrapper_binary(candidate),
    )
    .map(|path| path.to_string_lossy().into_owned())
}

/// Discover runtime host: [`RUNTIME_HOST_ENV`], [`COMMERCIAL_BIN_ENV`], `PATH`, `~/.cargo/bin`, `~/.susi/bin`.
#[must_use]
pub fn runtime_host_executable() -> Option<PathBuf> {
    for env_name in [RUNTIME_HOST_ENV, COMMERCIAL_BIN_ENV] {
        if let Some(explicit) = non_empty_env(env_name).map(PathBuf::from) {
            return explicit.is_file().then_some(explicit);
        }
    }
    let fallbacks = home_dir()
        .into_iter()
        .flat_map(|h| [".cargo/bin", ".susi/bin"].map(|s| h.join(s)))
        .flat_map(|d| RUNTIME_HOST_BINS.map(|n| d.join(n)));
    find_program(
        std::env::var_os("PATH").as_deref(),
        &RUNTIME_HOST_BINS,
        fallbacks,
        |candidate| candidate.is_file() && !is_susi_wrapper_binary(candidate),
    )
}

/// Compatibility alias for [`runtime_host_executable`].
#[must_use]
pub fn commercial_executable() -> Option<PathBuf> {
    runtime_host_executable()
}

/// Git's `gpg.program`: delegates to `<runtime-host> vault gpg-program` if present, else real `gpg`.
pub fn run_git_gpg_program(args: &[String]) -> i32 {
    if let Some(program) = runtime_host_executable() {
        if let Ok(status) = Command::new(&program)
            .args(["vault", "gpg-program"])
            .args(args)
            .status()
        {
            return status.code().unwrap_or(1);
        }
    }
    let Some(real_gpg) = resolve_real_gpg_binary() else {
        eprintln!("susi-gpg: real `gpg` executable not found in PATH or standard directories");
        return 127;
    };
    Command::new(&real_gpg)
        .args(args)
        .status()
        .map_or(1, |status| status.code().unwrap_or(1))
}

/// Fast-path `argv[0]` multicall dispatch when invoked as `susi-gpg` / `susi-git-gpg`.
#[must_use]
pub fn dispatch_gpg_program_multicall() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os();
    let argv0 = args.next()?;
    let stem = Path::new(&argv0)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    let stem = stem.strip_suffix(".exe").unwrap_or(&stem);
    if !matches!(stem, "susi-gpg" | "susi-git-gpg" | "susi-gpg-program") {
        return None;
    }
    let Ok(rest) = args.map(|a| a.into_string()).collect::<Result<Vec<_>, _>>() else {
        eprintln!("susi-gpg: command arguments must be valid UTF-8");
        return Some(std::process::ExitCode::from(2));
    };
    Some(std::process::ExitCode::from(
        u8::try_from(run_git_gpg_program(&rest)).unwrap_or(1),
    ))
}
