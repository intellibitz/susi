//! Cross-OS KeePassXC `.kdbx` bridge, Google Drive sync helper, and scoped child execution.
//!
//! Replaces OS-locked PowerShell (`AgentVault.ps1` + DPAPI-on-`.kdbx`) and
//! Linux-only bash shims (`ai-keys-sync` / `ai-keys-run` + empty-password KWallet)
//! with a single cross-platform pipeline usable on Windows, CachyOS Linux,
//! headless services, background agents, and cloud containers.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

const KDBX_MAGIC: [u8; 4] = [0x03, 0xd9, 0xa2, 0x9a];
const KEEPASSXC_TIMEOUT: Duration = Duration::from_secs(30);
const RCLONE_TIMEOUT: Duration = Duration::from_secs(120);

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::cloud_env::parse_env_file;
use super::json_util::create_private_dir;
use super::vault_store::{
    drop_secret_string, enroll_master_key, resolve_master_key, resolve_unlock_secret, SealedVault,
    VaultConfig, PURPOSE_KDBX,
};

/// Explicit env override selecting the KeePassXC `.kdbx` vault (or `"off"`).
pub const VAULT_DB_ENV: &str = "SUSI_VAULT_DB";
/// Shared commercial env override selecting the KeePassXC `.kdbx` vault (or `"off"`).
pub const COMMERCIAL_VAULT_ENV: &str = "SUSI_COMMERCIAL_VAULT";
/// Portable KeePassXC key file env override.
pub const VAULT_KEY_FILE_ENV: &str = "SUSI_COMMERCIAL_VAULT_KEY_FILE";
/// Shared key file env override.
pub const SUSI_VAULT_KEY_FILE_ENV: &str = "SUSI_VAULT_KDBX_KEY_FILE";
/// Explicit `keepassxc-cli` executable override.
pub const KEEPASSXC_CLI_ENV: &str = "SUSI_KEEPASSXC_CLI";

/// Canonical commercial authority entry in `Automation.kdbx`.
pub const COMMERCIAL_AUTHORITY_ENTRY: &str = "App credentials/Susi commercial authority";
/// Canonical customer license signing entry in `Automation.kdbx`.
pub const COMMERCIAL_SIGNING_ENTRY: &str = "App credentials/Susi customer license signing";

/// Sync direction between local `.kdbx` and Google Drive mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GDriveSyncDirection {
    Auto,
    Pull,
    Push,
}

impl GDriveSyncDirection {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "newer" => Ok(Self::Auto),
            "pull" | "from-drive" | "download" => Ok(Self::Pull),
            "push" | "to-drive" | "upload" => Ok(Self::Push),
            other => Err(format!(
                "invalid Google Drive sync direction `{other}` (expected auto, pull, or push)"
            )),
        }
    }
}

/// Summary of Google Drive `.kdbx` synchronization (paths/hashes only, never secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GDriveSyncReport {
    pub action: String,
    pub local_path: PathBuf,
    pub remote_desc: String,
    pub backup_path: Option<PathBuf>,
}

/// Summary of KeePassXC -> SealedVault synchronization (counts/keys only, never secret values).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdbxSyncReport {
    pub db_path: PathBuf,
    pub gdrive_report: Option<GDriveSyncReport>,
    pub imported_cloud_keys: Vec<String>,
    pub imported_commercial_keys: Vec<String>,
    pub total_sealed_entries: usize,
    pub dry_run: bool,
}

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

fn validate_non_personal(path: &Path) -> Result<(), String> {
    if is_personal_vault_path(path) {
        return Err(format!(
            "refusing Personal KeePassXC vault `{}`; Personal vaults are off limits to Susi and agents",
            path.display()
        ));
    }
    Ok(())
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

/// Default `.kdbx` search order across Linux, Windows, and local Google Drive sync folders.
#[must_use]
pub fn default_vault_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(h) = home {
        out.push(h.join("Vault").join("Automation.kdbx"));
        out.push(
            h.join("KeePassXC-AppLauncher")
                .join("store")
                .join("Automation.kdbx"),
        );
        out.push(
            h.join("Google Drive")
                .join("My Drive")
                .join("Vault")
                .join("Automation.kdbx"),
        );
        out.push(h.join("Google Drive").join("Vault").join("Automation.kdbx"));
        out.push(h.join("Google Drive").join("Automation.kdbx"));
        out.push(h.join("Drive").join("Vault").join("Automation.kdbx"));
        out.push(h.join("Vault").join("AI-Keys.kdbx"));
    }
    #[cfg(windows)]
    {
        out.push(PathBuf::from(r"G:\My Drive\Vault\Automation.kdbx"));
        out.push(PathBuf::from(r"G:\My Drive\Automation.kdbx"));
    }
    out
}

/// Discover an existing non-Personal `.kdbx` database from env, `vault.json`, or standard paths.
pub fn discover_kdbx_path(config_dir: &Path) -> Result<Option<PathBuf>, String> {
    for env_name in [VAULT_DB_ENV, COMMERCIAL_VAULT_ENV] {
        if let Some(val) = non_empty_env(env_name) {
            if matches!(val.to_ascii_lowercase().as_str(), "off" | "none") {
                return Ok(None);
            }
            let path = PathBuf::from(&val);
            validate_non_personal(&path)?;
            if !path.is_file() {
                return Err(format!(
                    "configured KeePassXC database `{}` from {env_name} does not exist",
                    path.display()
                ));
            }
            return Ok(Some(path));
        }
    }

    let cfg = VaultConfig::load(config_dir);
    if let Some(configured) = cfg.vault_db_path {
        validate_non_personal(&configured)?;
        if configured.is_file() {
            return Ok(Some(configured));
        }
    }

    let mut fallback_file: Option<PathBuf> = None;
    for candidate in default_vault_candidates(home_dir().as_deref()) {
        if !is_personal_vault_path(&candidate) && candidate.is_file() {
            if has_valid_kdbx_magic(&candidate) {
                return Ok(Some(candidate));
            }
            if fallback_file.is_none() {
                fallback_file = Some(candidate);
            }
        }
    }
    Ok(fallback_file)
}

fn has_valid_kdbx_magic(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && magic == KDBX_MAGIC
}

/// Discover optional synced `.keyx` / `.key` keyfile for KeePassXC unlock.
#[must_use]
pub fn discover_kdbx_key_file(config_dir: &Path) -> Option<PathBuf> {
    for env_name in [SUSI_VAULT_KEY_FILE_ENV, VAULT_KEY_FILE_ENV] {
        if let Some(val) = non_empty_env(env_name) {
            let p = PathBuf::from(val);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    VaultConfig::load(config_dir)
        .key_file_path
        .filter(|p| p.is_file())
}

fn resolve_keepassxc_cli(config_dir: &Path) -> String {
    if let Some(cli) = non_empty_env(KEEPASSXC_CLI_ENV) {
        return cli;
    }
    if let Some(cli) = VaultConfig::load(config_dir).keepassxc_cli {
        if !cli.trim().is_empty() {
            return cli;
        }
    }
    #[cfg(windows)]
    {
        let standard = Path::new(r"C:\Program Files\KeePassXC\keepassxc-cli.exe");
        if standard.is_file() {
            return standard.to_string_lossy().into_owned();
        }
    }
    "keepassxc-cli".to_string()
}

struct KdbxAuthMaterial {
    passphrase: Option<String>,
    key_file: Option<PathBuf>,
    interactive_tty: bool,
}

impl Drop for KdbxAuthMaterial {
    fn drop(&mut self) {
        if let Some(pass) = self.passphrase.take() {
            drop_secret_string(pass);
        }
    }
}

fn resolve_kdbx_auth(
    config_dir: &Path,
    allow_interactive: bool,
) -> Result<KdbxAuthMaterial, String> {
    let key_file = discover_kdbx_key_file(config_dir);
    let passphrase = match resolve_unlock_secret(config_dir, PURPOSE_KDBX)? {
        Some((p, _)) if !p.trim().is_empty() => Some(p),
        _ => None,
    };
    let interactive_tty = allow_interactive
        && !susi_paths::console::is_unattended_env()
        && passphrase.is_none()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal();

    if passphrase.is_none() && key_file.is_none() && !interactive_tty {
        return Err(
            "KeePassXC unlock material is not enrolled for non-interactive use. Run `susi keys vault enroll --prompt-kdbx` once on this host, or set SUSI_VAULT_KDBX_PASSPHRASE / SUSI_COMMERCIAL_VAULT_KEY_FILE."
                .to_string(),
        );
    }
    Ok(KdbxAuthMaterial {
        passphrase,
        key_file,
        interactive_tty,
    })
}

fn run_keepassxc(
    cli: &str,
    subargs: &[&str],
    db_path: &Path,
    extra_args: &[&str],
    auth: &KdbxAuthMaterial,
) -> Result<String, String> {
    let mut cmd = Command::new(cli);
    cmd.args(subargs);
    if auth.passphrase.is_some() {
        cmd.arg("-q");
    } else if auth.key_file.is_some() && !auth.interactive_tty {
        cmd.arg("--no-password").arg("-q");
    }
    if let Some(kf) = &auth.key_file {
        cmd.arg("-k").arg(kf);
    }
    cmd.arg("--").arg(db_path);
    cmd.args(extra_args);

    let out = if let Some(pass) = &auth.passphrase {
        let mut stdin_line = String::with_capacity(pass.len() + 1);
        stdin_line.push_str(pass);
        stdin_line.push('\n');
        let res = susi_paths::console::run_output_within(
            &mut cmd,
            Some(stdin_line.as_bytes()),
            KEEPASSXC_TIMEOUT,
        );
        drop_secret_string(stdin_line);
        res.map_err(|e| format!("failed to run `{cli}`: {e}"))?
    } else if auth.interactive_tty {
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("failed to run `{cli}`: {e}"))?
    } else {
        susi_paths::console::run_output_within(&mut cmd, None, KEEPASSXC_TIMEOUT)
            .map_err(|e| format!("failed to run `{cli}`: {e}"))?
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {:?}", out.status.code())
        } else {
            stderr
        };
        return Err(format!("`{cli}` failed on {}: {detail}", db_path.display()));
    }
    String::from_utf8(out.stdout).map_err(|_| "keepassxc-cli emitted non-UTF-8 output".to_string())
}

/// List non-group entries in a KeePassXC database (`ls -R -f`).
pub fn list_kdbx_entries(
    config_dir: &Path,
    db_path: &Path,
    allow_interactive: bool,
) -> Result<Vec<String>, String> {
    validate_non_personal(db_path)?;
    let cli = resolve_keepassxc_cli(config_dir);
    let auth = resolve_kdbx_auth(config_dir, allow_interactive)?;
    let stdout = run_keepassxc(&cli, &["ls", "-R", "-f"], db_path, &[], &auth)?;
    let mut entries = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim().trim_start_matches('/');
        if trimmed.is_empty() || trimmed.ends_with('/') {
            continue;
        }
        // Skip Recycle Bin entries so deleted/rotated credentials never come back.
        if trimmed.to_ascii_lowercase().starts_with("recycle bin/") {
            continue;
        }
        entries.push(trimmed.to_string());
    }
    Ok(entries)
}

/// Read one attribute (`Password` or `Notes`) of an entry in `db_path`.
pub fn read_kdbx_attribute(
    config_dir: &Path,
    db_path: &Path,
    entry: &str,
    attribute: &str,
    allow_interactive: bool,
) -> Result<String, String> {
    validate_non_personal(db_path)?;
    let clean_entry = entry.trim();
    if clean_entry.is_empty() || clean_entry.starts_with('-') {
        return Err("invalid KeePassXC entry name".to_string());
    }
    let cli = resolve_keepassxc_cli(config_dir);
    let auth = resolve_kdbx_auth(config_dir, allow_interactive)?;
    let stdout = run_keepassxc(
        &cli,
        &["show", "-s", "-a", attribute],
        db_path,
        &[clean_entry],
        &auth,
    )?;
    Ok(stdout.trim_end_matches(&['\r', '\n'][..]).to_string())
}

/// Map a KeePassXC entry path to `(scope, key_name)` targets in `SealedVault`.
#[must_use]
pub fn map_kdbx_entry(entry_path: &str) -> Vec<(&'static str, String)> {
    let trimmed = entry_path.trim().trim_start_matches('/');
    let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed).trim();
    let norm = leaf.to_ascii_lowercase();
    let mut out = Vec::new();

    match norm.as_str() {
        "susi commercial authority" | "susi_commercial_secret" | "commercial authority" => {
            out.push(("commercial", "SUSI_COMMERCIAL_SECRET".to_string()));
        }
        "susi customer license signing"
        | "susi_customer_license_signing_key"
        | "customer license signing" => {
            out.push((
                "commercial",
                "SUSI_CUSTOMER_LICENSE_SIGNING_KEY".to_string(),
            ));
        }
        "openai" | "openai_api_key" => out.push(("cloud", "OPENAI_API_KEY".to_string())),
        "anthropic" | "claude" | "anthropic_api_key" => {
            out.push(("cloud", "ANTHROPIC_API_KEY".to_string()))
        }
        "gemini" | "google_gemini" | "gemini_api_key" => {
            out.push(("cloud", "GEMINI_API_KEY".to_string()))
        }
        "deepseek" | "deepseek_api_key" => out.push(("cloud", "DEEPSEEK_API_KEY".to_string())),
        "kimi" | "moonshot" | "moonshot_api_key" => {
            out.push(("cloud", "MOONSHOT_API_KEY".to_string()))
        }
        "openrouter" | "openrouter_api_key" => {
            out.push(("cloud", "OPENROUTER_API_KEY".to_string()))
        }
        "fireworks" | "fireworks_api_key" => out.push(("cloud", "FIREWORKS_API_KEY".to_string())),
        "together" | "together_api_key" => out.push(("cloud", "TOGETHER_API_KEY".to_string())),
        "xai" | "grok" | "xai_api_key" => out.push(("cloud", "XAI_API_KEY".to_string())),
        "groq" | "groq_api_key" => out.push(("cloud", "GROQ_API_KEY".to_string())),
        "deepinfra" | "deepinfra_api_key" => out.push(("cloud", "DEEPINFRA_API_KEY".to_string())),
        "mistral" | "vibe" | "mistral_api_key" => {
            out.push(("cloud", "MISTRAL_API_KEY".to_string()))
        }
        "minimax" | "minimax_api_key" => out.push(("cloud", "MINIMAX_API_KEY".to_string())),
        "cerebras" | "cerebras_api_key" => out.push(("cloud", "CEREBRAS_API_KEY".to_string())),
        "cohere" | "cohere_api_key" => out.push(("cloud", "COHERE_API_KEY".to_string())),
        "perplexity" | "perplexity_api_key" => {
            out.push(("cloud", "PERPLEXITY_API_KEY".to_string()))
        }
        "hf" | "huggingface" | "hf_token" | "huggingface_hub_token" => {
            out.push(("cloud", "HF_TOKEN".to_string()));
            out.push(("cloud", "HUGGINGFACE_HUB_TOKEN".to_string()));
        }
        "gh" | "github" | "gh_token" | "github_token" | "github_personal_access_token" => {
            out.push(("cloud", "GH_TOKEN".to_string()));
            out.push(("cloud", "GITHUB_TOKEN".to_string()));
            out.push(("cloud", "GITHUB_PERSONAL_ACCESS_TOKEN".to_string()));
        }
        "brave" | "brave_api_key" => out.push(("cloud", "BRAVE_API_KEY".to_string())),
        "tavily" | "tavily_api_key" => out.push(("cloud", "TAVILY_API_KEY".to_string())),
        "context7" | "context7_api_key" => out.push(("cloud", "CONTEXT7_API_KEY".to_string())),
        _ => {
            if is_env_var_name(leaf) {
                out.push(("cloud", leaf.to_string()));
            }
        }
    }

    if !trimmed.is_empty() {
        out.push(("kdbx", trimmed.to_string()));
    }
    out
}

fn is_env_var_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_sha256(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().to_vec())
}

fn atomic_copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    let bytes = std::fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    if let Some(parent) = dst.parent() {
        create_private_dir(parent).map_err(|e| format!("create dir {}: {e}", parent.display()))?;
    }
    super::json_util::atomic_write_bytes(dst, &bytes)
        .map_err(|e| format!("write {}: {e}", dst.display()))
}

/// Synchronize a local `.kdbx` with a configured Google Drive sync folder or `rclone` remote.
/// Never truncates live databases cleanly without first writing a timestamped backup.
pub fn sync_google_drive(
    config_dir: &Path,
    local_db: &Path,
    direction: GDriveSyncDirection,
    dry_run: bool,
) -> Result<Option<GDriveSyncReport>, String> {
    validate_non_personal(local_db)?;
    let cfg = VaultConfig::load(config_dir);

    if let Some(remote_spec) = cfg
        .rclone_remote
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if remote_spec.to_ascii_lowercase().contains("personal") {
            return Err("refusing Personal remote KeePassXC database".to_string());
        }
        return sync_via_rclone(local_db, remote_spec, direction, dry_run).map(Some);
    }

    let Some(gdrive_path) = cfg.gdrive_sync_path else {
        return Ok(None);
    };
    validate_non_personal(&gdrive_path)?;
    if gdrive_path == local_db {
        return Ok(Some(GDriveSyncReport {
            action: "same-file".to_string(),
            local_path: local_db.to_path_buf(),
            remote_desc: gdrive_path.display().to_string(),
            backup_path: None,
        }));
    }

    let local_exists = local_db.is_file();
    let remote_exists = gdrive_path.is_file();
    if !local_exists && !remote_exists {
        return Err(format!(
            "neither local `{}` nor Google Drive mirror `{}` exists",
            local_db.display(),
            gdrive_path.display()
        ));
    }

    if local_exists && remote_exists {
        let h_local = file_sha256(local_db)?;
        let h_remote = file_sha256(&gdrive_path)?;
        if h_local == h_remote {
            return Ok(Some(GDriveSyncReport {
                action: "in-sync".to_string(),
                local_path: local_db.to_path_buf(),
                remote_desc: gdrive_path.display().to_string(),
                backup_path: None,
            }));
        }
    }

    let chosen = match direction {
        GDriveSyncDirection::Pull => "pull",
        GDriveSyncDirection::Push => "push",
        GDriveSyncDirection::Auto => {
            if !local_exists {
                "pull"
            } else if !remote_exists {
                "push"
            } else {
                let m_local = std::fs::metadata(local_db).and_then(|m| m.modified()).ok();
                let m_remote = std::fs::metadata(&gdrive_path)
                    .and_then(|m| m.modified())
                    .ok();
                if m_remote > m_local {
                    "pull"
                } else {
                    "push"
                }
            }
        }
    };

    if dry_run {
        return Ok(Some(GDriveSyncReport {
            action: format!("dry-run:{chosen}"),
            local_path: local_db.to_path_buf(),
            remote_desc: gdrive_path.display().to_string(),
            backup_path: None,
        }));
    }

    let backup_path = if chosen == "pull" && local_exists {
        let bak = PathBuf::from(format!("{}.bak.{}", local_db.display(), now_unix()));
        atomic_copy_file(local_db, &bak)?;
        Some(bak)
    } else if chosen == "push" && remote_exists {
        let bak = PathBuf::from(format!("{}.bak.{}", gdrive_path.display(), now_unix()));
        atomic_copy_file(&gdrive_path, &bak)?;
        Some(bak)
    } else {
        None
    };

    if chosen == "pull" {
        atomic_copy_file(&gdrive_path, local_db)?;
    } else {
        atomic_copy_file(local_db, &gdrive_path)?;
    }

    Ok(Some(GDriveSyncReport {
        action: chosen.to_string(),
        local_path: local_db.to_path_buf(),
        remote_desc: gdrive_path.display().to_string(),
        backup_path,
    }))
}

fn sync_via_rclone(
    local_db: &Path,
    remote_spec: &str,
    direction: GDriveSyncDirection,
    dry_run: bool,
) -> Result<GDriveSyncReport, String> {
    let dir_flag = match direction {
        GDriveSyncDirection::Auto | GDriveSyncDirection::Pull => "pull",
        GDriveSyncDirection::Push => "push",
    };
    let backup = if dir_flag == "pull" && local_db.is_file() && !dry_run {
        let bak = PathBuf::from(format!("{}.bak.{}", local_db.display(), now_unix()));
        atomic_copy_file(local_db, &bak)?;
        Some(bak)
    } else {
        None
    };

    let mut args = vec!["copyto"];
    if dry_run {
        args.push("--dry-run");
    }
    let local_str = local_db.to_string_lossy().into_owned();
    let (src, dst) = if dir_flag == "push" {
        (local_str.as_str(), remote_spec)
    } else {
        (remote_spec, local_str.as_str())
    };
    let mut rclone_cmd = Command::new("rclone");
    rclone_cmd.args(args).arg(src).arg(dst);
    susi_paths::console::apply_non_interactive_env(&mut rclone_cmd);
    let out = susi_paths::console::run_output_within(&mut rclone_cmd, None, RCLONE_TIMEOUT)
        .map_err(|e| format!("failed to run `rclone`: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("rclone copyto failed: {err}"));
    }
    Ok(GDriveSyncReport {
        action: if dry_run {
            format!("dry-run:rclone-{dir_flag}")
        } else {
            format!("rclone-{dir_flag}")
        },
        local_path: local_db.to_path_buf(),
        remote_desc: remote_spec.to_string(),
        backup_path: backup,
    })
}

/// Sync Google Drive (when configured) and import KeePassXC entries into `SealedVault`.
pub fn sync_kdbx_to_sealed_vault(
    config_dir: &Path,
    db_override: Option<&Path>,
    gdrive_direction: GDriveSyncDirection,
    allow_interactive: bool,
    dry_run: bool,
) -> Result<KdbxSyncReport, String> {
    let db_path = if let Some(explicit) = db_override {
        validate_non_personal(explicit)?;
        explicit.to_path_buf()
    } else {
        discover_kdbx_path(config_dir)?.ok_or_else(|| {
            "no Automation/AI-Keys KeePassXC database found; pass --db <path> or restore Automation.kdbx from Google Drive"
                .to_string()
        })?
    };

    let gdrive_report = sync_google_drive(config_dir, &db_path, gdrive_direction, dry_run)?;
    if !db_path.is_file() {
        return Err(format!(
            "KeePassXC database missing at {}",
            db_path.display()
        ));
    }

    if !dry_run && resolve_master_key(config_dir)?.is_none() {
        enroll_master_key(config_dir, None, None)?;
    }

    let mut vault = if dry_run {
        SealedVault::open(config_dir)?.unwrap_or_default()
    } else {
        SealedVault::open_or_init(config_dir)?
    };

    let mut imported_cloud = BTreeSet::new();
    let mut imported_commercial = BTreeSet::new();

    if let Ok(csv_records) = export_kdbx_csv_records(config_dir, &db_path, allow_interactive) {
        if !csv_records.is_empty() {
            for rec in csv_records {
                import_kdbx_record(
                    &mut vault,
                    &rec,
                    &mut imported_cloud,
                    &mut imported_commercial,
                );
                drop_secret_string(rec.password);
                drop_secret_string(rec.notes);
            }
        }
    } else {
        let entries = list_kdbx_entries(config_dir, &db_path, allow_interactive)?;
        for entry in &entries {
            let leaf = entry.rsplit('/').next().unwrap_or(entry).trim();
            let pw =
                read_kdbx_attribute(config_dir, &db_path, entry, "Password", allow_interactive)?;
            let notes = if leaf.eq_ignore_ascii_case("Forward to apps") {
                read_kdbx_attribute(config_dir, &db_path, entry, "Notes", allow_interactive)
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let rec = CsvKdbxRecord {
                entry_path: entry.clone(),
                password: pw,
                notes,
            };
            import_kdbx_record(
                &mut vault,
                &rec,
                &mut imported_cloud,
                &mut imported_commercial,
            );
            drop_secret_string(rec.password);
            drop_secret_string(rec.notes);
        }
    }

    let total = vault.entry_count();
    if !dry_run {
        vault.save(config_dir)?;
        let mut cfg = VaultConfig::load(config_dir);
        cfg.vault_db_path = Some(db_path.clone());
        cfg.last_sync_unix = Some(now_unix());
        cfg.synced_entry_count = total;
        cfg.save(config_dir)?;
    }

    Ok(KdbxSyncReport {
        db_path,
        gdrive_report,
        imported_cloud_keys: imported_cloud.into_iter().collect(),
        imported_commercial_keys: imported_commercial.into_iter().collect(),
        total_sealed_entries: total,
        dry_run,
    })
}

struct CsvKdbxRecord {
    entry_path: String,
    password: String,
    notes: String,
}

fn import_kdbx_record(
    vault: &mut SealedVault,
    rec: &CsvKdbxRecord,
    imported_cloud: &mut BTreeSet<String>,
    imported_commercial: &mut BTreeSet<String>,
) {
    let entry_path = rec.entry_path.as_str();
    let pw = rec.password.as_str();
    let notes = rec.notes.as_str();
    let leaf = entry_path.rsplit('/').next().unwrap_or(entry_path).trim();
    if !pw.trim().is_empty() {
        for (scope, name) in map_kdbx_entry(entry_path) {
            vault.set(scope, &name, pw.trim());
            if scope == "cloud" {
                imported_cloud.insert(name);
            } else if scope == "commercial" {
                imported_commercial.insert(name);
            }
        }
    }
    if leaf.eq_ignore_ascii_case("Forward to apps") && !notes.trim().is_empty() {
        for (env_k, env_v) in parse_env_file(notes) {
            if !env_k.trim().is_empty() && !env_v.trim().is_empty() {
                let scope = if env_k.starts_with("SUSI_COMMERCIAL")
                    || env_k.starts_with("SUSI_CUSTOMER_LICENSE")
                {
                    imported_commercial.insert(env_k.clone());
                    "commercial"
                } else {
                    imported_cloud.insert(env_k.clone());
                    "cloud"
                };
                vault.set(scope, &env_k, &env_v);
            }
        }
    }
}

fn export_kdbx_csv_records(
    config_dir: &Path,
    db_path: &Path,
    allow_interactive: bool,
) -> Result<Vec<CsvKdbxRecord>, String> {
    validate_non_personal(db_path)?;
    let cli = resolve_keepassxc_cli(config_dir);
    let auth = resolve_kdbx_auth(config_dir, allow_interactive)?;
    let mut csv_text = run_keepassxc(&cli, &["export", "-f", "csv"], db_path, &[], &auth)?;
    let rows = parse_csv_rows(&csv_text);
    drop_secret_string(std::mem::take(&mut csv_text));
    let Some(header) = rows.first() else {
        return Ok(Vec::new());
    };
    let col = |name: &str| header.iter().position(|h| h.eq_ignore_ascii_case(name));
    let group_idx = col("Group");
    let title_idx = col("Title").ok_or_else(|| "CSV missing Title column".to_string())?;
    let pass_idx = col("Password").ok_or_else(|| "CSV missing Password column".to_string())?;
    let notes_idx = col("Notes");

    let mut out = Vec::new();
    for row in rows.into_iter().skip(1) {
        let title = row.get(title_idx).map(|s| s.trim()).unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let raw_group = group_idx
            .and_then(|i| row.get(i))
            .map(|s| s.trim().trim_matches('/'))
            .unwrap_or_default();
        let norm_group = raw_group
            .strip_prefix("Root/")
            .or_else(|| raw_group.strip_prefix("Root"))
            .unwrap_or(raw_group)
            .trim_start_matches('/');
        if norm_group.to_ascii_lowercase().starts_with("recycle bin") {
            continue;
        }
        let entry_path = if norm_group.is_empty() {
            title.to_string()
        } else {
            format!("{norm_group}/{title}")
        };
        let password = row.get(pass_idx).cloned().unwrap_or_default();
        let notes = notes_idx
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_default();
        out.push(CsvKdbxRecord {
            entry_path,
            password,
            notes,
        });
    }
    Ok(out)
}

fn parse_csv_rows(input: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = input.chars().peekable();
    let mut in_quotes = false;

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' if field.is_empty() => in_quotes = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn file_mtime_unix(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Strictly non-interactive auto-refresh: if KeePassXC unlock material and an enrolled DB (or GDrive
/// mirror) are configured and newer than `last_sync_unix`, pull + re-seal without ever prompting.
pub fn auto_sync_if_enrolled(config_dir: &Path) -> Result<bool, String> {
    if resolve_master_key(config_dir)?.is_none() {
        return Ok(false);
    }
    let has_kdbx_auth = resolve_unlock_secret(config_dir, PURPOSE_KDBX)
        .ok()
        .flatten()
        .is_some()
        || discover_kdbx_key_file(config_dir).is_some();
    if !has_kdbx_auth {
        return Ok(false);
    }
    let cfg = VaultConfig::load(config_dir);
    let Some(db_path) = cfg
        .vault_db_path
        .clone()
        .filter(|p| p.is_file() && !is_personal_vault_path(p) && has_valid_kdbx_magic(p))
    else {
        return Ok(false);
    };
    let last_sync = cfg.last_sync_unix.unwrap_or(0);
    let local_mtime = file_mtime_unix(&db_path).unwrap_or(0);
    let drive_mtime = cfg
        .gdrive_sync_path
        .as_deref()
        .and_then(file_mtime_unix)
        .unwrap_or(0);
    let vault_exists = SealedVault::path_in(config_dir).is_file();
    if vault_exists && local_mtime <= last_sync && drive_mtime <= last_sync {
        return Ok(false);
    }
    sync_kdbx_to_sealed_vault(
        config_dir,
        Some(&db_path),
        GDriveSyncDirection::Pull,
        false,
        false,
    )?;
    Ok(true)
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

/// Resolve client-scoped environment pairs from `SealedVault` without global shell export.
pub fn resolve_client_env(
    config_dir: &Path,
    client: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    let vault = SealedVault::open(config_dir)?.ok_or_else(|| {
        "no sealed vault found; run `susi keys vault enroll` and `susi keys vault sync` first"
            .to_string()
    })?;
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
        "aider" | "cline" | "opencode" | "kilo" | "kilocode" | "sweagent" | "agency-swarm"
        | "interpreter" | "pi" | "hermes" | "hermes-agent" => Some(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "DEEPSEEK_API_KEY",
            "GROQ_API_KEY",
            "OPENROUTER_API_KEY",
            "MISTRAL_API_KEY",
            "XAI_API_KEY",
            "ZHIPU_API_KEY",
            "ZAI_API_KEY",
        ]),
        "deepseek" => Some(&["DEEPSEEK_API_KEY"]),
        "openai" => Some(&["OPENAI_API_KEY"]),
        "qwen" => Some(&["DASHSCOPE_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]),
        "litellm" | "litellm-proxy" | "gateway" => Some(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "DEEPSEEK_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "DEEPINFRA_API_KEY",
            "SILICONFLOW_API_KEY",
            "MOONSHOT_API_KEY",
            "OPENROUTER_API_KEY",
            "ZHIPU_API_KEY",
            "ZAI_API_KEY",
            "HF_TOKEN",
        ]),
        "tiny-agents" => Some(&["HF_TOKEN", "HUGGINGFACE_HUB_TOKEN", "OPENAI_API_KEY"]),
        "llm" | "cloud" | "commercial" | "all" => {
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
                    "unknown vault exec client profile `{other}` (expected claude, codex, gemini, gh, hf, vibe, aider, cline, opencode, qwen, gateway, cloud, commercial, or all)"
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

    for scope in ["env", "cloud"] {
        for (k, v) in vault.scope_entries(scope) {
            let allowed = wild_all
                || dynamic_list
                    .as_ref()
                    .is_some_and(|list| list.iter().any(|entry| entry == &k))
                || (dynamic_list.is_none()
                    && builtin.is_none_or(|list| list.contains(&k.as_str())));
            if !v.is_empty() && allowed {
                env_out.insert(k, v);
            }
        }
    }
    if matches!(profile.as_str(), "commercial" | "all") {
        for (k, v) in vault.scope_entries("commercial") {
            if !v.is_empty() {
                env_out.insert(k, v);
            }
        }
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
    susi_paths::console::apply_non_interactive_env(&mut cmd);
    let status = cmd
        .status()
        .map_err(|e| format!("failed to spawn `{program}`: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
// Unit-test module only; production code in this module uses explicit Result handling.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_kdbx_bridge_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn personal_vault_paths_are_always_refused() {
        for path in [
            "/home/user/Personal.kdbx",
            "/tmp/ceo-PERSONAL-vault.kdbx",
            "C:\\Users\\ceo\\Personal\\Automation.kdbx",
        ] {
            let p = Path::new(path);
            assert!(is_personal_vault_path(p));
            assert!(validate_non_personal(p).is_err());
        }
        assert!(!is_personal_vault_path(Path::new(
            "/home/user/Vault/Automation.kdbx"
        )));
        assert!(!is_personal_vault_path(Path::new(
            "/home/user/Vault/AI-Keys.kdbx"
        )));
    }

    #[test]
    fn entry_mapping_covers_commercial_and_cloud_providers() {
        let auth = map_kdbx_entry("App credentials/Susi commercial authority");
        assert!(auth.contains(&("commercial", "SUSI_COMMERCIAL_SECRET".to_string())));

        let sign = map_kdbx_entry("App credentials/Susi customer license signing");
        assert!(sign.contains(&(
            "commercial",
            "SUSI_CUSTOMER_LICENSE_SIGNING_KEY".to_string()
        )));

        let openai = map_kdbx_entry("AI/openai");
        assert!(openai.contains(&("cloud", "OPENAI_API_KEY".to_string())));
        assert!(openai.contains(&("kdbx", "AI/openai".to_string())));
    }

    #[test]
    fn gdrive_mirror_sync_pulls_newer_remote_with_local_backup() {
        let dir = scratch_dir("gdrive_pull");
        let cfg_dir = dir.join("config");
        let local_db = dir.join("Vault").join("Automation.kdbx");
        let drive_db = dir.join("Google Drive").join("Automation.kdbx");
        std::fs::create_dir_all(local_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(drive_db.parent().unwrap()).unwrap();

        std::fs::write(&local_db, b"old-local-kdbx").unwrap();
        std::fs::write(&drive_db, b"fresh-drive-kdbx").unwrap();

        let cfg = VaultConfig {
            gdrive_sync_path: Some(drive_db.clone()),
            ..Default::default()
        };
        cfg.save(&cfg_dir).unwrap();

        let report = sync_google_drive(&cfg_dir, &local_db, GDriveSyncDirection::Pull, false)
            .unwrap()
            .unwrap();
        assert_eq!(report.action, "pull");
        assert!(report.backup_path.as_ref().is_some_and(|p| p.is_file()));
        assert_eq!(std::fs::read(&local_db).unwrap(), b"fresh-drive-kdbx");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_client_env_filters_secrets_by_client_profile() {
        let _guard = crate::env_test_lock();
        let dir = scratch_dir("exec_profile");
        let prev = std::env::var_os(super::super::vault_store::VAULT_KEY_ENV);
        std::env::remove_var(super::super::vault_store::VAULT_KEY_ENV);

        enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        let mut vault = SealedVault::open_or_init(&dir).unwrap();
        vault.set("cloud", "OPENAI_API_KEY", "sk-openai-1");
        vault.set("cloud", "GEMINI_API_KEY", "gem-2");
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

        if let Some(v) = prev {
            std::env::set_var(super::super::vault_store::VAULT_KEY_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_kdbx_to_sealed_vault_imports_real_keepassxc_without_tty() {
        let cli_ok = Command::new("keepassxc-cli")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !cli_ok {
            return;
        }
        let _guard = crate::env_test_lock();
        let dir = scratch_dir("real_kdbx");
        let cfg_dir = dir.join("cfg");
        let db = dir.join("Automation.kdbx");
        let kf = dir.join("automation.key");
        std::fs::write(&kf, b"portable-cross-os-keyfile-material").unwrap();

        let create = Command::new("keepassxc-cli")
            .args(["db-create", "-k"])
            .arg(&kf)
            .arg(&db)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(create.status.success(), "db-create: {:?}", create);

        for group in ["AI", "App credentials"] {
            let mk = Command::new("keepassxc-cli")
                .args(["mkdir", "--no-password", "-k"])
                .arg(&kf)
                .arg(&db)
                .arg(group)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                mk.status.success(),
                "mkdir {group}: {}",
                String::from_utf8_lossy(&mk.stderr)
            );
        }

        for (entry, pw) in [
            ("AI/openai", "sk-real-kdbx-openai"),
            (
                COMMERCIAL_AUTHORITY_ENTRY,
                "comm-real-kdbx-authority-secret",
            ),
        ] {
            let mut child = Command::new("keepassxc-cli")
                .args(["add", "--no-password", "-k"])
                .arg(&kf)
                .args(["-p", "--"])
                .arg(&db)
                .arg(entry)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(format!("{pw}\n").as_bytes());
            }
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "add {entry}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let prev_key = std::env::var_os(super::super::vault_store::VAULT_KEY_ENV);
        let prev_kf = std::env::var_os(VAULT_KEY_FILE_ENV);
        let prev_kf2 = std::env::var_os(SUSI_VAULT_KEY_FILE_ENV);
        std::env::remove_var(super::super::vault_store::VAULT_KEY_ENV);
        std::env::set_var(SUSI_VAULT_KEY_FILE_ENV, &kf);

        let report =
            sync_kdbx_to_sealed_vault(&cfg_dir, Some(&db), GDriveSyncDirection::Auto, false, false)
                .unwrap();
        assert!(report
            .imported_cloud_keys
            .contains(&"OPENAI_API_KEY".to_string()));
        assert!(report
            .imported_commercial_keys
            .contains(&"SUSI_COMMERCIAL_SECRET".to_string()));

        let vault = SealedVault::open(&cfg_dir).unwrap().unwrap();
        assert_eq!(
            vault.get("cloud", "OPENAI_API_KEY"),
            Some("sk-real-kdbx-openai")
        );
        assert_eq!(
            vault.get("commercial", "SUSI_COMMERCIAL_SECRET"),
            Some("comm-real-kdbx-authority-secret")
        );

        match prev_key {
            Some(v) => std::env::set_var(super::super::vault_store::VAULT_KEY_ENV, v),
            None => std::env::remove_var(super::super::vault_store::VAULT_KEY_ENV),
        }
        match prev_kf {
            Some(v) => std::env::set_var(VAULT_KEY_FILE_ENV, v),
            None => std::env::remove_var(VAULT_KEY_FILE_ENV),
        }
        match prev_kf2 {
            Some(v) => std::env::set_var(SUSI_VAULT_KEY_FILE_ENV, v),
            None => std::env::remove_var(SUSI_VAULT_KEY_FILE_ENV),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn csv_parser_and_custom_client_conf_work() {
        let rows = parse_csv_rows(
            "\"Group\",\"Title\",\"Password\",\"Notes\"\n\"Root/AI\",\"openai\",\"sk-\"\"q\"\",1\",\"line1\nline2\"\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], "Root/AI");
        assert_eq!(rows[1][1], "openai");
        assert_eq!(rows[1][2], "sk-\"q\",1");
        assert_eq!(rows[1][3], "line1\nline2");

        let _guard = crate::env_test_lock();
        let dir = scratch_dir("custom_client_conf");
        let prev = std::env::var_os(super::super::vault_store::VAULT_KEY_ENV);
        std::env::remove_var(super::super::vault_store::VAULT_KEY_ENV);

        enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        let mut vault = SealedVault::open_or_init(&dir).unwrap();
        vault.set("cloud", "SILICONFLOW_API_KEY", "sf-1");
        vault.set("cloud", "OPENAI_API_KEY", "sk-2");
        vault.save(&dir).unwrap();

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

        if let Some(v) = prev {
            std::env::set_var(super::super::vault_store::VAULT_KEY_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
