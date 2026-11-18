//! Installed runtime discovery and offline GPG multicall fallback.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Names the installed runtime explicitly, ahead of PATH.
pub const COMMERCIAL_BIN_ENV: &str = "SUSI_COMMERCIAL_BIN";

#[doc(hidden)]
pub fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[doc(hidden)]
pub fn home_dir() -> Option<PathBuf> {
    non_empty_env("HOME")
        .or_else(|| non_empty_env("USERPROFILE"))
        .map(PathBuf::from)
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
    // A Windows-style path reaches a Unix host through shared Git config, where
    // `Path::file_name` does not split on a backslash; take the last component
    // after either separator on every platform.
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
pub fn resolve_non_wrapper_executable(
    env_override: Option<&str>,
    exe_names: &[&str],
    extra_candidates: &[PathBuf],
    skip_wrappers: bool,
) -> Option<String> {
    if let Some(raw) = env_override.map(str::trim).filter(|s| !s.is_empty()) {
        let p = Path::new(raw);
        if (!skip_wrappers || !is_susi_wrapper_binary(p))
            && (p.is_file() || !raw.contains(['/', '\\']))
        {
            return Some(raw.to_string());
        }
    }
    find_program(
        std::env::var_os("PATH").as_deref(),
        exe_names,
        extra_candidates.iter().cloned(),
        |candidate| candidate.is_file() && (!skip_wrappers || !is_susi_wrapper_binary(candidate)),
    )
    .map(|path| path.to_string_lossy().into_owned())
}

#[doc(hidden)]
pub fn resolve_real_gpg_binary() -> Option<String> {
    #[cfg(windows)]
    let extras = {
        let mut paths = vec![
            PathBuf::from(r"C:\Program Files\GnuPG\bin\gpg.exe"),
            PathBuf::from(r"C:\Program Files (x86)\GnuPG\bin\gpg.exe"),
            PathBuf::from(r"C:\Program Files\Git\usr\bin\gpg.exe"),
        ];
        if let Some(home) = home_dir() {
            paths.push(home.join(r"scoop\apps\gpg\current\bin\gpg.exe"));
        }
        paths
    };
    #[cfg(target_os = "macos")]
    let extras = [
        "/opt/homebrew/bin/gpg",
        "/usr/local/bin/gpg",
        "/usr/local/MacGPG2/bin/gpg",
    ]
    .map(PathBuf::from);
    #[cfg(all(unix, not(target_os = "macos")))]
    let extras = ["/usr/bin/gpg", "/usr/bin/gpg2", "/usr/local/bin/gpg"].map(PathBuf::from);
    #[cfg(windows)]
    let names = ["gpg.exe", "gpg2.exe"];
    #[cfg(not(windows))]
    let names = ["gpg", "gpg2"];
    resolve_non_wrapper_executable(
        non_empty_env("SUSI_REAL_GPG").as_deref(),
        &names,
        &extras,
        true,
    )
}

/// The `susi-commercial` executable that serves the KeePassXC verbs: [`COMMERCIAL_BIN_ENV`],
/// then `PATH`, then `~/.cargo/bin` and `~/.susi/bin`.
#[must_use]
pub fn commercial_executable() -> Option<PathBuf> {
    if let Some(explicit) = non_empty_env(COMMERCIAL_BIN_ENV).map(PathBuf::from) {
        return explicit.is_file().then_some(explicit);
    }
    let name = format!("susi-commercial{}", std::env::consts::EXE_SUFFIX);
    let fallbacks = home_dir()
        .map(|h| {
            vec![
                h.join(".cargo").join("bin").join(&name),
                h.join(".susi").join("bin").join(&name),
            ]
        })
        .unwrap_or_default();
    find_program(
        std::env::var_os("PATH").as_deref(),
        &[name.as_str()],
        fallbacks,
        |candidate| candidate.is_file() && !is_susi_wrapper_binary(candidate),
    )
}

/// What `susi keys vault <verb>` says when `susi-commercial` is not installed.
#[must_use]
pub fn moved_verb_message(verb: &str) -> String {
    format!(
        "`susi keys vault {verb}` is served by Susi Commercial Apps: install it and run \
         `susi-commercial vault {verb}` (or `susi keys vault {verb}`, which forwards to it)"
    )
}

/// Core's database precedence for a forwarded verb: an explicit `--db`, then
/// `SUSI_VAULT_DB`, then `SUSI_COMMERCIAL_VAULT`. `susi-commercial` reads `vault.json`
/// before the environment, so the forwarder names the environment's database itself.
/// `Err` when the environment switches the KeePassXC vault off.
pub fn forwarded_database(explicit: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    for name in ["SUSI_VAULT_DB", "SUSI_COMMERCIAL_VAULT"] {
        if let Some(value) = non_empty_env(name) {
            if matches!(value.to_ascii_lowercase().as_str(), "off" | "none") {
                return Err(format!(
                    "the KeePassXC vault is switched off by {name}={value}"
                ));
            }
            return Ok(Some(PathBuf::from(value)));
        }
    }
    Ok(None)
}

/// Runs `susi-commercial vault <args>` on this process's terminal and returns its exit
/// code; `envs` reach only the child. `Err` (naming the command) when it is not installed.
pub fn run_commercial_vault(args: &[String], envs: &[(&str, &str)]) -> Result<i32, String> {
    let verb = args.first().map_or("status", String::as_str);
    let mut forwarded = Vec::with_capacity(args.len() + 1);
    forwarded.push("vault".to_string());
    forwarded.extend_from_slice(args);
    if commercial_executable().is_none() {
        return Err(moved_verb_message(verb));
    }
    run_commercial(&forwarded, envs)
}

/// Runs `susi-commercial <args>` (a capability that moved out of core, Mandate 84) on this
/// process's terminal and working directory and returns its exit code; `envs` reach only
/// the child. `Err` when `susi-commercial` is not installed or cannot start.
pub fn run_commercial(args: &[String], envs: &[(&str, &str)]) -> Result<i32, String> {
    let program = commercial_executable()
        .ok_or_else(|| "susi-commercial is not installed here".to_string())?;
    let status = Command::new(&program)
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to run `{}`: {e}", program.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// [`run_commercial`] in `cwd`, for verbs that act on the caller's workspace.
pub fn run_commercial_in(
    args: &[String],
    cwd: &Path,
    envs: &[(&str, &std::ffi::OsStr)],
) -> Result<i32, String> {
    let program = commercial_executable()
        .ok_or_else(|| "susi-commercial is not installed here".to_string())?;
    let status = Command::new(&program)
        .args(args)
        .envs(envs.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to run `{}`: {e}", program.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// Asks the installed `susi-commercial vault auto-sync --json` to refresh the sealed vault
/// from the enrolled KeePassXC database (no prompt, output captured). `Ok(None)` when it is
/// not installed.
pub fn commercial_auto_sync() -> Result<Option<bool>, String> {
    let Some(program) = commercial_executable() else {
        return Ok(None);
    };
    let out = Command::new(&program)
        .args(["vault", "auto-sync", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run `{}`: {e}", program.display()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("unreadable `vault auto-sync --json` answer: {e}"))?;
    Ok(answer["refreshed"].as_bool())
}

/// Git's `gpg.program`. `susi-commercial vault gpg-program` primes the signing passphrase
/// from the sealed vault and runs the real `gpg`; without it, the real `gpg` runs directly
/// (an agent that already holds the passphrase, or a pinentry, still signs).
pub fn run_git_gpg_program(args: &[String]) -> i32 {
    if commercial_executable().is_some() {
        let mut forwarded = Vec::with_capacity(args.len() + 1);
        forwarded.push("gpg-program".to_string());
        forwarded.extend_from_slice(args);
        match run_commercial_vault(&forwarded, &[]) {
            Ok(code) => return code,
            Err(e) => eprintln!("susi-gpg: {e}; running gpg without vault priming"),
        }
    }
    let Some(real_gpg) = resolve_real_gpg_binary() else {
        eprintln!("susi-gpg: real `gpg` executable not found in PATH or standard directories");
        return 127;
    };
    match Command::new(&real_gpg)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("susi-gpg: failed to execute `{real_gpg}`: {e}");
            1
        }
    }
}

/// Fast-path `argv[0]` multicall dispatch when invoked as `susi-gpg` / `susi-git-gpg`
/// (Linux installs the wrapper as a symlink to `susi`, so this must keep answering).
#[must_use]
pub fn dispatch_gpg_program_multicall() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os();
    let argv0 = args.next()?;
    let path = Path::new(&argv0);
    let stem_lower = path.file_name().and_then(|n| n.to_str()).map(|s| {
        let l = s.to_ascii_lowercase();
        l.strip_suffix(".exe").unwrap_or(&l).to_string()
    })?;
    if !matches!(
        stem_lower.as_str(),
        "susi-gpg" | "susi-git-gpg" | "susi-gpg-program"
    ) {
        return None;
    }
    let Ok(rest) = args
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("susi-gpg: command arguments must be valid UTF-8");
        return Some(std::process::ExitCode::from(2));
    };
    let code = run_git_gpg_program(&rest);
    Some(std::process::ExitCode::from(
        u8::try_from(code).unwrap_or(1),
    ))
}
