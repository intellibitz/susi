//! Scoped child execution from the sealed vault, and the hand-off of the
//! KeePassXC, Google Drive and GPG signing verbs to Susi Commercial Apps.
//!
//! `susi keys vault exec` runs a child with client-scoped secrets from the
//! sealed vault ([`super::vault_store`]); that stays in core. The KeePassXC
//! bridge (sync, doctor, prime-signing, setup-git, rekey, gpg-program and the
//! daemon's unattended refresh) is served by `susi-commercial vault` and its
//! plug host. This module finds that executable, keeps core's database
//! precedence for the verbs it forwards, and keeps a `susi-gpg` wrapper working
//! without it by running the real `gpg`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use super::vault_store::SealedVault;

/// Plane-bus topic the plug host answers with `{"refreshed": bool}`: refresh the
/// sealed vault from the enrolled KeePassXC database without prompting.
pub const TOPIC_KDBX_AUTO_SYNC: &str = "vault.kdbx.auto_sync.v1";
/// Names the `susi-commercial` executable explicitly, ahead of `PATH`.
pub const COMMERCIAL_BIN_ENV: &str = "SUSI_COMMERCIAL_BIN";

/// Check the Personal vault boundary (AGENTS.md §6): never open Personal vaults.
#[must_use]
pub fn is_personal_vault_path(path: &Path) -> bool {
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        if stem.to_ascii_lowercase().contains("personal") {
            return true;
        }
    }
    path.components().any(|c| match c {
        Component::Normal(part) => part
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("personal"),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            false
        }
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn home_dir() -> Option<PathBuf> {
    non_empty_env("HOME")
        .or_else(|| non_empty_env("USERPROFILE"))
        .map(PathBuf::from)
}

/// Search each PATH directory in order before inspecting installation-specific fallbacks.
fn find_program(
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

fn load_client_conf_allowlist(config_dir: &Path, profile: &str) -> Option<Vec<String>> {
    let mut candidates = vec![config_dir.join("vault-clients.conf")];
    if let Some(h) = home_dir() {
        candidates.push(h.join(".config").join("ai-keys").join("clients.conf"));
    }
    for path in candidates {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((client, keys)) = trimmed.split_once('=') else {
                continue;
            };
            if client.trim().eq_ignore_ascii_case(profile) {
                return Some(
                    keys.split_whitespace()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                );
            }
        }
    }
    None
}

/// Resolve client-scoped environment pairs from `SealedVault` (or cloud overlay fallback) without global shell export.
/// Callers that want the enrolled KeePassXC database reflected first refresh the sealed vault
/// through [`TOPIC_KDBX_AUTO_SYNC`] or [`commercial_auto_sync`] before calling this.
pub fn resolve_client_env(
    config_dir: &Path,
    client: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    let mut env_out = BTreeMap::new();
    let profile = client
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("all")
        .to_ascii_lowercase();

    let mut wild_all = false;
    let mut dynamic_list: Option<Vec<String>> = None;

    let builtin: Option<&[&str]> = match profile.as_str() {
        "claude" => Some(&[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_WORKSPACE_ID",
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "FIREWORKS_API_KEY",
            "CONTEXT7_API_KEY",
            "BRAVE_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_PERSONAL_ACCESS_TOKEN",
        ]),
        "codex" => Some(&[
            "OPENAI_API_KEY",
            "CONTEXT7_API_KEY",
            "BRAVE_API_KEY",
            "TAVILY_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_PERSONAL_ACCESS_TOKEN",
        ]),
        "gemini" => Some(&[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "CONTEXT7_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
        ]),
        "gh" => Some(&["GH_TOKEN", "GITHUB_TOKEN", "GITHUB_PERSONAL_ACCESS_TOKEN"]),
        "hf" | "huggingface-cli" => Some(&["HF_TOKEN", "HUGGINGFACE_HUB_TOKEN"]),
        "vibe" | "vibe-acp" | "vibe-app-server" => Some(&[
            "MISTRAL_API_KEY",
            "DEEPSEEK_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
        ]),
        "goose" | "factory" | "droid" => Some(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_WORKSPACE_ID",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "DEEPSEEK_API_KEY",
            "GROQ_API_KEY",
            "OPENROUTER_API_KEY",
            "MISTRAL_API_KEY",
            "XAI_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_API_KEY",
            "DASHSCOPE_API_KEY",
            "ZHIPU_API_KEY",
            "ZAI_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "CONTEXT7_API_KEY",
            "BRAVE_API_KEY",
        ]),
        "aider" | "cline" | "opencode" | "kilo" | "kilocode" | "sweagent" | "agency-swarm"
        | "interpreter" | "pi" | "hermes" | "hermes-agent" => Some(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_WORKSPACE_ID",
            "GEMINI_API_KEY",
            "DEEPSEEK_API_KEY",
            "GROQ_API_KEY",
            "OPENROUTER_API_KEY",
            "MISTRAL_API_KEY",
            "XAI_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_API_KEY",
            "DASHSCOPE_API_KEY",
            "ZHIPU_API_KEY",
            "ZAI_API_KEY",
        ]),
        "deepseek" => Some(&["DEEPSEEK_API_KEY"]),
        "openai" => Some(&["OPENAI_API_KEY"]),
        "kimi" => Some(&["MOONSHOT_API_KEY", "KIMI_API_KEY", "OPENAI_API_KEY"]),
        "glm" => Some(&["ZHIPU_API_KEY", "ZAI_API_KEY"]),
        "qwen" => Some(&["DASHSCOPE_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]),
        "litellm" | "litellm-proxy" | "gateway" => Some(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_WORKSPACE_ID",
            "GEMINI_API_KEY",
            "DEEPSEEK_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "DEEPINFRA_API_KEY",
            "SILICONFLOW_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_API_KEY",
            "OPENROUTER_API_KEY",
            "ZHIPU_API_KEY",
            "ZAI_API_KEY",
            "HF_TOKEN",
            "LITELLM_MASTER_KEY",
        ]),
        "tiny-agents" => Some(&["HF_TOKEN", "HUGGINGFACE_HUB_TOKEN", "OPENAI_API_KEY"]),
        "susi" | "llm" | "cloud" | "commercial" | "all" => {
            wild_all = true;
            None
        }
        other => {
            if let Some(conf_list) = load_client_conf_allowlist(config_dir, other) {
                if conf_list.iter().any(|k| k == "*") {
                    wild_all = true;
                } else {
                    dynamic_list = Some(conf_list);
                }
                None
            } else {
                return Err(format!(
                    "unknown vault exec client profile `{other}` (expected claude, codex, gemini, goose, factory, gh, hf, vibe, aider, cline, opencode, kimi, glm, qwen, gateway, susi, cloud, commercial, or all)"
                ));
            }
        }
    };

    // Custom clients.conf entry for a built-in profile can refine or widen it cleanly.
    if !wild_all && dynamic_list.is_none() {
        if let Some(conf_list) = load_client_conf_allowlist(config_dir, &profile) {
            if conf_list.iter().any(|k| k == "*") {
                wild_all = true;
            } else if !conf_list.is_empty() {
                dynamic_list = Some(conf_list);
            }
        }
    }

    let allow_key = |k: &str| -> bool {
        wild_all
            || dynamic_list
                .as_ref()
                .is_some_and(|list| list.iter().any(|entry| entry == k))
            || (dynamic_list.is_none() && builtin.is_none_or(|list| list.contains(&k)))
    };

    if let Some(vault) = SealedVault::open(config_dir)? {
        for scope in ["env", "cloud"] {
            for (k, v) in vault.scope_entries(scope) {
                if !v.is_empty() && allow_key(&k) {
                    env_out.insert(k, v);
                }
            }
        }
        if matches!(profile.as_str(), "commercial" | "susi" | "all") {
            for (k, v) in vault.scope_entries("commercial") {
                if !v.is_empty() {
                    env_out.insert(k, v);
                }
            }
        }
        return Ok(env_out);
    }

    for (k, v) in crate::cloud_env::load_cloud_env_from(config_dir) {
        if !v.is_empty() && allow_key(&k) {
            env_out.insert(k, v);
        }
    }
    if let Some(list) = dynamic_list.as_deref() {
        for k in list {
            if let Ok(v) = std::env::var(k) {
                let t = v.trim();
                if !t.is_empty() {
                    env_out.entry(k.clone()).or_insert_with(|| t.to_string());
                }
            }
        }
    } else if let Some(list) = builtin {
        for &k in list {
            if let Ok(v) = std::env::var(k) {
                let t = v.trim();
                if !t.is_empty() {
                    env_out
                        .entry(k.to_string())
                        .or_insert_with(|| t.to_string());
                }
            }
        }
    }
    if env_out.is_empty() {
        return Err(
            "no sealed vault found; run `susi keys vault enroll` and `susi keys vault sync` first"
                .to_string(),
        );
    }
    Ok(env_out)
}

/// Execute `command[0]` with `command[1..]` inheriting client-scoped secrets from `SealedVault`.
pub fn exec_with_client_env(
    config_dir: &Path,
    client: Option<&str>,
    command: &[String],
) -> Result<i32, String> {
    let Some(program) = command.first() else {
        return Err("missing command after `--`".to_string());
    };
    let secrets = resolve_client_env(config_dir, client)?;
    let mut cmd = Command::new(program);
    cmd.args(&command[1..]);
    cmd.envs(&secrets);
    cmd.env("SUSI_UNATTENDED", "1");
    susi_paths::console::apply_non_interactive_env(&mut cmd);
    let status = cmd
        .status()
        .map_err(|e| format!("failed to spawn `{program}`: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

fn is_susi_wrapper_binary(path: &Path) -> bool {
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

fn resolve_non_wrapper_executable(
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

fn resolve_real_gpg_binary() -> Option<String> {
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

#[cfg(test)]
// Unit-test module only; production code in this module uses explicit Result handling.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::vault_store::{enroll_master_key, VAULT_KEY_ENV};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_vault_exec_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Clears `keys` for one test and restores them on drop.
    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl EnvRestore {
        fn clear(keys: &[&'static str]) -> Self {
            Self(
                keys.iter()
                    .map(|&k| {
                        let prev = std::env::var_os(k);
                        std::env::remove_var(k);
                        (k, prev)
                    })
                    .collect(),
            )
        }
    }
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (k, v) in self.0.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn vault_exec_personal_vault_paths_are_always_refused() {
        for path in [
            "/home/user/Personal.kdbx",
            "/tmp/ceo-PERSONAL-vault.kdbx",
            "C:\\Users\\ceo\\Personal\\Automation.kdbx",
        ] {
            assert!(is_personal_vault_path(Path::new(path)), "{path}");
        }
        assert!(!is_personal_vault_path(Path::new(
            "/home/user/Vault/Automation.kdbx"
        )));
        assert!(!is_personal_vault_path(Path::new(
            "/home/user/Vault/AI-Keys.kdbx"
        )));
    }

    #[test]
    fn vault_exec_client_profiles_scope_sealed_secrets() {
        let _guard = crate::env_test_lock();
        let _env = EnvRestore::clear(&[VAULT_KEY_ENV]);
        let dir = scratch_dir("profiles");
        enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        let mut vault = SealedVault::open_or_init(&dir).unwrap();
        vault.set("cloud", "OPENAI_API_KEY", "sk-openai-1");
        vault.set("cloud", "GEMINI_API_KEY", "gem-2");
        vault.set("cloud", "SILICONFLOW_API_KEY", "sf-1");
        vault.set("commercial", "SUSI_COMMERCIAL_SECRET", "comm-3");
        vault.save(&dir).unwrap();

        let codex = resolve_client_env(&dir, Some("codex")).unwrap();
        assert_eq!(
            codex.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-openai-1")
        );
        assert!(!codex.contains_key("GEMINI_API_KEY"));
        assert!(!codex.contains_key("SUSI_COMMERCIAL_SECRET"));
        let comm = resolve_client_env(&dir, Some("commercial")).unwrap();
        assert_eq!(
            comm.get("SUSI_COMMERCIAL_SECRET").map(String::as_str),
            Some("comm-3")
        );

        // A custom vault-clients.conf profile narrows to exactly its keys.
        std::fs::write(
            dir.join("vault-clients.conf"),
            "customtool = SILICONFLOW_API_KEY\n",
        )
        .unwrap();
        let custom = resolve_client_env(&dir, Some("customtool")).unwrap();
        assert_eq!(
            custom.get("SILICONFLOW_API_KEY").map(String::as_str),
            Some("sf-1")
        );
        assert!(!custom.contains_key("OPENAI_API_KEY"));
        assert!(resolve_client_env(&dir, Some("nobody-knows-this")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_exec_without_a_sealed_vault_falls_back_to_cloud_env() {
        let _guard = crate::env_test_lock();
        let _env = EnvRestore::clear(&[VAULT_KEY_ENV]);
        let dir = scratch_dir("cloud_env");
        std::fs::write(
            dir.join("cloud.env"),
            "ANTHROPIC_API_KEY=sk-ant-fallback\nANTHROPIC_WORKSPACE_ID=ws-1\n",
        )
        .unwrap();
        let goose = resolve_client_env(&dir, Some("goose")).unwrap();
        assert_eq!(
            goose.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant-fallback")
        );
        assert_eq!(
            goose.get("ANTHROPIC_WORKSPACE_ID").map(String::as_str),
            Some("ws-1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_exec_gpg_lookup_never_picks_a_susi_wrapper() {
        assert!(is_susi_wrapper_binary(Path::new(
            "/home/user/.susi/bin/susi-gpg"
        )));
        assert!(is_susi_wrapper_binary(Path::new(r"C:\susi-gpg.exe")));
        assert!(is_susi_wrapper_binary(Path::new(
            r"C:\Users\u\.susi\bin\susi.exe"
        )));
        assert!(!is_susi_wrapper_binary(Path::new("/usr/bin/gpg")));
        assert!(!is_susi_wrapper_binary(Path::new("susi-commercial.exe")));
    }

    #[test]
    fn vault_exec_forwarded_database_keeps_core_precedence() {
        let _guard = crate::env_test_lock();
        let _env = EnvRestore::clear(&["SUSI_VAULT_DB", "SUSI_COMMERCIAL_VAULT"]);
        assert_eq!(forwarded_database(None).unwrap(), None);
        std::env::set_var("SUSI_COMMERCIAL_VAULT", "/v/commercial.kdbx");
        assert_eq!(
            forwarded_database(None).unwrap(),
            Some(PathBuf::from("/v/commercial.kdbx"))
        );
        std::env::set_var("SUSI_VAULT_DB", "/v/shared.kdbx");
        assert_eq!(
            forwarded_database(None).unwrap(),
            Some(PathBuf::from("/v/shared.kdbx"))
        );
        let explicit = Some(PathBuf::from("/v/explicit.kdbx"));
        assert_eq!(forwarded_database(explicit.clone()).unwrap(), explicit);
        std::env::set_var("SUSI_VAULT_DB", "off");
        assert!(forwarded_database(None)
            .unwrap_err()
            .contains("switched off"));
    }

    #[test]
    fn vault_exec_an_absent_commercial_install_names_the_command() {
        let _guard = crate::env_test_lock();
        let _env = EnvRestore::clear(&[COMMERCIAL_BIN_ENV]);
        std::env::set_var(
            COMMERCIAL_BIN_ENV,
            scratch_dir("no_bin").join("missing.exe"),
        );
        assert_eq!(commercial_executable(), None);
        let err = run_commercial_vault(&["doctor".to_string()], &[]).unwrap_err();
        assert!(err.contains("`susi-commercial vault doctor`"), "{err}");
        assert_eq!(commercial_auto_sync(), Ok(None));
    }
}
