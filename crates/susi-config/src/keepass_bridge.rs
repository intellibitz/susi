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

#[cfg(any(target_os = "macos", test))]
fn append_macos_cloud_drive_candidates(home: &Path, out: &mut Vec<PathBuf>) {
    let cloud_storage = home.join("Library").join("CloudStorage");
    if let Ok(entries) = std::fs::read_dir(&cloud_storage) {
        let mut drive_dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("GoogleDrive"))
            })
            .collect();
        drive_dirs.sort();
        for drive in drive_dirs {
            let my_drive = drive.join("My Drive");
            out.push(my_drive.join("Vault").join("Automation.kdbx"));
            out.push(my_drive.join("Vault").join("AI-Keys.kdbx"));
            out.push(my_drive.join("Automation.kdbx"));
            out.push(my_drive.join("AI-Keys.kdbx"));
        }
    }
}

#[cfg(any(windows, test))]
fn windows_keepassxc_candidates(
    program_files: Option<&str>,
    program_w6432: Option<&str>,
    program_arm: Option<&str>,
    local_app_data: Option<&str>,
    user_profile: Option<&str>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in [
        program_files,
        program_w6432,
        program_arm,
        Some(r"C:\Program Files"),
        Some(r"C:\Program Files (x86)"),
    ]
    .into_iter()
    .flatten()
    {
        let trimmed = base.trim();
        if !trimmed.is_empty() {
            out.push(
                PathBuf::from(trimmed)
                    .join("KeePassXC")
                    .join("keepassxc-cli.exe"),
            );
        }
    }
    if let Some(lad) = local_app_data.map(str::trim).filter(|s| !s.is_empty()) {
        let base = PathBuf::from(lad);
        out.push(
            base.join("Programs")
                .join("KeePassXC")
                .join("keepassxc-cli.exe"),
        );
        out.push(
            base.join("Microsoft")
                .join("WinGet")
                .join("Links")
                .join("keepassxc-cli.exe"),
        );
    }
    if let Some(up) = user_profile.map(str::trim).filter(|s| !s.is_empty()) {
        out.push(
            PathBuf::from(up)
                .join("scoop")
                .join("apps")
                .join("keepassxc")
                .join("current")
                .join("keepassxc-cli.exe"),
        );
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn macos_keepassxc_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = vec![
        PathBuf::from("/Applications/KeePassXC.app/Contents/MacOS/keepassxc-cli"),
        PathBuf::from("/opt/homebrew/bin/keepassxc-cli"),
        PathBuf::from("/usr/local/bin/keepassxc-cli"),
        PathBuf::from("/opt/local/bin/keepassxc-cli"),
    ];
    if let Some(h) = home {
        out.insert(
            1,
            h.join("Applications")
                .join("KeePassXC.app")
                .join("Contents")
                .join("MacOS")
                .join("keepassxc-cli"),
        );
    }
    out
}

fn resolve_rclone_cli() -> String {
    if let Some(cli) = non_empty_env("SUSI_RCLONE_BIN") {
        return cli;
    }
    #[cfg(target_os = "macos")]
    for candidate in [
        "/opt/homebrew/bin/rclone",
        "/usr/local/bin/rclone",
        "/opt/local/bin/rclone",
    ] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    #[cfg(windows)]
    {
        for candidate in [
            non_empty_env("LOCALAPPDATA")
                .map(|b| PathBuf::from(b).join(r"Microsoft\WinGet\Links\rclone.exe")),
            non_empty_env("USERPROFILE").map(|b| PathBuf::from(b).join(r"scoop\shims\rclone.exe")),
            Some(PathBuf::from(r"C:\ProgramData\chocolatey\bin\rclone.exe")),
        ]
        .into_iter()
        .flatten()
        {
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "rclone".to_string()
}

/// Linux `keepassxc-cli` native/Snap/Linuxbrew/Nix executable search candidates.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn linux_keepassxc_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = vec![
        PathBuf::from("/usr/bin/keepassxc-cli"),
        PathBuf::from("/usr/local/bin/keepassxc-cli"),
        PathBuf::from("/snap/bin/keepassxc.cli"),
        PathBuf::from("/snap/bin/keepassxc-cli"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/keepassxc-cli"),
        PathBuf::from("/run/current-system/sw/bin/keepassxc-cli"),
    ];
    if let Some(h) = home {
        out.push(h.join(".local").join("bin").join("keepassxc-cli"));
        out.push(h.join(".nix-profile").join("bin").join("keepassxc-cli"));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeepassCliSpec {
    program: String,
    prefix_args: Vec<String>,
    is_flatpak: bool,
    display: String,
}

const KEEPASSXC_FLATPAK_APP_ID: &str = "org.keepassxc.KeePassXC";

fn parse_keepassxc_cli_spec(raw: &str) -> KeepassCliSpec {
    let trimmed = raw.trim();
    if let Some(app_id) = trimmed.strip_prefix("flatpak:") {
        let app = if app_id.trim().is_empty() {
            KEEPASSXC_FLATPAK_APP_ID
        } else {
            app_id.trim()
        };
        return KeepassCliSpec {
            program: "flatpak".to_string(),
            prefix_args: vec![
                "run".to_string(),
                "--command=keepassxc-cli".to_string(),
                app.to_string(),
            ],
            is_flatpak: true,
            display: format!("flatpak:{app}"),
        };
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() >= 2 && (parts[0] == "flatpak" || parts[0].ends_with("/flatpak")) {
        return KeepassCliSpec {
            program: parts[0].to_string(),
            prefix_args: parts[1..].iter().map(|s| (*s).to_string()).collect(),
            is_flatpak: true,
            display: trimmed.to_string(),
        };
    }
    KeepassCliSpec {
        program: trimmed.to_string(),
        prefix_args: Vec::new(),
        is_flatpak: false,
        display: trimmed.to_string(),
    }
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn detect_linux_keepassxc_spec_with<F, D, K>(
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
    is_file: F,
    is_dir: D,
    flatpak_installed: K,
) -> KeepassCliSpec
where
    F: Fn(&Path) -> bool,
    D: Fn(&Path) -> bool,
    K: Fn(&str) -> bool,
{
    if let Some(raw) = path_var {
        for dir in std::env::split_paths(raw) {
            for bin_name in ["keepassxc-cli", "keepassxc.cli"] {
                let candidate = dir.join(bin_name);
                if is_file(&candidate) {
                    let s = candidate.to_string_lossy().into_owned();
                    return parse_keepassxc_cli_spec(&s);
                }
            }
        }
    }
    for candidate in linux_keepassxc_candidates(home) {
        if is_file(&candidate) {
            let s = candidate.to_string_lossy().into_owned();
            return parse_keepassxc_cli_spec(&s);
        }
    }
    let mut flatpak_dirs = vec![
        PathBuf::from("/var/lib/flatpak/app").join(KEEPASSXC_FLATPAK_APP_ID),
        PathBuf::from("/var/lib/flatpak/exports/bin").join(KEEPASSXC_FLATPAK_APP_ID),
    ];
    if let Some(h) = home {
        flatpak_dirs.push(
            h.join(".local")
                .join("share")
                .join("flatpak")
                .join("app")
                .join(KEEPASSXC_FLATPAK_APP_ID),
        );
        flatpak_dirs.push(
            h.join(".local")
                .join("share")
                .join("flatpak")
                .join("exports")
                .join("bin")
                .join(KEEPASSXC_FLATPAK_APP_ID),
        );
    }
    let has_flatpak_export = flatpak_dirs.iter().any(|p| is_dir(p) || is_file(p))
        || flatpak_installed(KEEPASSXC_FLATPAK_APP_ID);
    if has_flatpak_export {
        let flatpak_bin = path_var
            .and_then(|v| {
                std::env::split_paths(v)
                    .map(|d| d.join("flatpak"))
                    .find(|p| is_file(p))
            })
            .or_else(|| {
                let usr = PathBuf::from("/usr/bin/flatpak");
                is_file(&usr).then_some(usr)
            })
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "flatpak".to_string());
        return KeepassCliSpec {
            program: flatpak_bin,
            prefix_args: vec![
                "run".to_string(),
                "--command=keepassxc-cli".to_string(),
                KEEPASSXC_FLATPAK_APP_ID.to_string(),
            ],
            is_flatpak: true,
            display: format!("flatpak:{KEEPASSXC_FLATPAK_APP_ID}"),
        };
    }
    parse_keepassxc_cli_spec("keepassxc-cli")
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn should_set_qt_offscreen(
    display: Option<&str>,
    wayland_display: Option<&str>,
    qt_qpa_platform: Option<&str>,
) -> bool {
    let empty = |v: Option<&str>| v.map(str::trim).is_none_or(str::is_empty);
    empty(display) && empty(wayland_display) && empty(qt_qpa_platform)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn is_headless_qt_env() -> bool {
    let display = std::env::var("DISPLAY").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let qpa = std::env::var("QT_QPA_PLATFORM").ok();
    should_set_qt_offscreen(display.as_deref(), wayland.as_deref(), qpa.as_deref())
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn is_headless_qt_env() -> bool {
    false
}

/// Default `.kdbx` search order across Linux, macOS, Windows, and local Google Drive sync folders.
#[must_use]
pub fn default_vault_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    default_vault_candidates_for(None, home)
}

fn default_vault_candidates_for(config_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(cfg) = config_dir {
        let vault_sub = cfg.join("vault");
        out.push(vault_sub.join("Automation.kdbx"));
        out.push(vault_sub.join("AI-Keys.kdbx"));
    }
    if let Some(h) = home {
        let susi_vault = h.join(".susi").join("vault");
        if config_dir.is_none_or(|c| c.join("vault") != susi_vault) {
            out.push(susi_vault.join("Automation.kdbx"));
            out.push(susi_vault.join("AI-Keys.kdbx"));
        }
        out.push(h.join("Vault").join("Automation.kdbx"));
        out.push(
            h.join("KeePassXC-AppLauncher")
                .join("store")
                .join("Automation.kdbx"),
        );
        out.push(h.join("Susi-Fleet-Vault").join("Automation.kdbx"));
        out.push(h.join("Susi-Fleet-Vault").join("AI-Keys.kdbx"));
        out.push(
            h.join("Google Drive")
                .join("My Drive")
                .join("Vault")
                .join("Automation.kdbx"),
        );
        out.push(
            h.join("Google Drive")
                .join("My Drive")
                .join("Vault")
                .join("AI-Keys.kdbx"),
        );
        out.push(
            h.join("Google Drive")
                .join("My Drive")
                .join("Susi-Fleet-Vault")
                .join("Automation.kdbx"),
        );
        out.push(
            h.join("Google Drive")
                .join("My Drive")
                .join("Susi-Fleet-Vault")
                .join("AI-Keys.kdbx"),
        );
        out.push(h.join("Google Drive").join("Vault").join("Automation.kdbx"));
        out.push(h.join("Google Drive").join("Vault").join("AI-Keys.kdbx"));
        out.push(h.join("Google Drive").join("Automation.kdbx"));
        out.push(h.join("Google Drive").join("AI-Keys.kdbx"));
        out.push(h.join("Drive").join("Vault").join("Automation.kdbx"));
        out.push(h.join("Drive").join("Vault").join("AI-Keys.kdbx"));
        out.push(
            h.join("Drive")
                .join("Susi-Fleet-Vault")
                .join("AI-Keys.kdbx"),
        );
        out.push(h.join("Vault").join("AI-Keys.kdbx"));
        #[cfg(target_os = "macos")]
        append_macos_cloud_drive_candidates(h, &mut out);
    }
    #[cfg(windows)]
    {
        out.push(PathBuf::from(r"G:\My Drive\Vault\Automation.kdbx"));
        out.push(PathBuf::from(r"G:\My Drive\Vault\AI-Keys.kdbx"));
        out.push(PathBuf::from(r"G:\My Drive\Susi-Fleet-Vault\AI-Keys.kdbx"));
        out.push(PathBuf::from(r"G:\My Drive\Automation.kdbx"));
        out.push(PathBuf::from(r"G:\My Drive\AI-Keys.kdbx"));
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
            if !has_valid_kdbx_magic(&path) {
                return Err(format!(
                    "configured KeePassXC database `{}` from {env_name} is corrupt or not a valid .kdbx file (invalid header magic)",
                    path.display()
                ));
            }
            return Ok(Some(path));
        }
    }

    let mut fallback_corrupt: Option<PathBuf> = None;
    let cfg = VaultConfig::load(config_dir);
    if let Some(configured) = cfg.vault_db_path {
        validate_non_personal(&configured)?;
        if configured.is_file() {
            if has_valid_kdbx_magic(&configured) {
                return Ok(Some(configured));
            }
            fallback_corrupt = Some(configured);
        }
    }

    for candidate in default_vault_candidates_for(Some(config_dir), home_dir().as_deref()) {
        if !is_personal_vault_path(&candidate) && candidate.is_file() {
            if has_valid_kdbx_magic(&candidate) {
                return Ok(Some(candidate));
            }
            if fallback_corrupt.is_none() {
                fallback_corrupt = Some(candidate);
            }
        }
    }
    Ok(fallback_corrupt)
}

fn has_valid_kdbx_magic(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && magic == KDBX_MAGIC
}

fn sidecar_keyfile_candidates_for(config_dir: Option<&Path>, db_path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(parent) = db_path.parent() else {
        return out;
    };
    let file_name = db_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = db_path.file_stem().and_then(|n| n.to_str()).unwrap_or("");

    for ext in ["keyx", "key"] {
        if !file_name.is_empty() {
            out.push(parent.join(format!("{file_name}.{ext}")));
        }
        if !stem.is_empty() {
            out.push(parent.join(format!("{stem}.{ext}")));
            let lower_stem = stem.to_ascii_lowercase();
            if lower_stem != stem {
                out.push(parent.join(format!("{lower_stem}.{ext}")));
            }
        }
    }
    if let Some(cfg) = config_dir {
        let vault_sub = cfg.join("vault");
        for ext in ["keyx", "key"] {
            if !stem.is_empty() {
                out.push(vault_sub.join(format!("{stem}.{ext}")));
                let lower_stem = stem.to_ascii_lowercase();
                if lower_stem != stem {
                    out.push(vault_sub.join(format!("{lower_stem}.{ext}")));
                }
            }
        }
    }
    out
}

/// Discover optional synced `.keyx` / `.key` keyfile for KeePassXC unlock.
#[must_use]
pub fn discover_kdbx_key_file(config_dir: &Path) -> Option<PathBuf> {
    discover_kdbx_key_file_for(config_dir, None)
}

fn discover_kdbx_key_file_for(config_dir: &Path, db_path: Option<&Path>) -> Option<PathBuf> {
    for env_name in [SUSI_VAULT_KEY_FILE_ENV, VAULT_KEY_FILE_ENV] {
        if let Some(val) = non_empty_env(env_name) {
            let p = PathBuf::from(val);
            if p.is_file() && !is_personal_vault_path(&p) {
                return Some(p);
            }
        }
    }
    if let Some(kf) = VaultConfig::load(config_dir)
        .key_file_path
        .filter(|p| p.is_file() && !is_personal_vault_path(p))
    {
        return Some(kf);
    }
    if let Some(db) = db_path {
        for cand in sidecar_keyfile_candidates_for(Some(config_dir), db) {
            if cand.is_file() && !is_personal_vault_path(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

fn resolve_keepassxc_cli_spec(config_dir: &Path) -> KeepassCliSpec {
    if let Some(cli) = non_empty_env(KEEPASSXC_CLI_ENV) {
        return parse_keepassxc_cli_spec(&cli);
    }
    if let Some(cli) = VaultConfig::load(config_dir).keepassxc_cli {
        if !cli.trim().is_empty() {
            return parse_keepassxc_cli_spec(&cli);
        }
    }
    #[cfg(windows)]
    {
        let pf = non_empty_env("ProgramFiles");
        let pw64 = non_empty_env("ProgramW6432");
        let parm = non_empty_env("ProgramFiles(Arm)");
        let lad = non_empty_env("LOCALAPPDATA");
        let up = non_empty_env("USERPROFILE");
        for standard in windows_keepassxc_candidates(
            pf.as_deref(),
            pw64.as_deref(),
            parm.as_deref(),
            lad.as_deref(),
            up.as_deref(),
        ) {
            if standard.is_file() {
                return parse_keepassxc_cli_spec(&standard.to_string_lossy());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let h = home_dir();
        for candidate in macos_keepassxc_candidates(h.as_deref()) {
            if candidate.is_file() {
                return parse_keepassxc_cli_spec(&candidate.to_string_lossy());
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let path_var = std::env::var_os("PATH");
        let h = home_dir();
        return detect_linux_keepassxc_spec_with(
            path_var.as_deref(),
            h.as_deref(),
            |p| p.is_file(),
            |p| p.is_dir(),
            |app_id| {
                let mut cmd = Command::new("flatpak");
                cmd.args(["info", app_id]);
                susi_paths::console::run_output_within(
                    &mut cmd,
                    None,
                    std::time::Duration::from_secs(5),
                )
                .is_ok_and(|o| o.status.success())
            },
        );
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    parse_keepassxc_cli_spec("keepassxc-cli")
}

fn resolve_keepassxc_cli(config_dir: &Path) -> String {
    resolve_keepassxc_cli_spec(config_dir).display
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

#[cfg(windows)]
fn is_isolated_temp_kdbx(db_path: &Path) -> bool {
    let tmp = std::env::temp_dir();
    let norm = |p: &Path| {
        p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_ascii_lowercase()
    };
    let db_norm = norm(db_path);
    let tmp_norm = norm(&tmp);
    !tmp_norm.is_empty() && db_norm.starts_with(&tmp_norm)
}

#[cfg(windows)]
fn resolve_windows_legacy_dpapi_passphrase(
    config_dir: &Path,
    db_path: &Path,
    has_key_file: bool,
) -> Option<String> {
    if is_personal_vault_path(db_path) {
        return None;
    }
    let stem = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stem != "automation" {
        return None;
    }
    let mut envelopes = Vec::new();
    if let Some(parent) = db_path.parent() {
        envelopes.push(parent.join("automation-key.dpapi"));
    }
    if !has_key_file && !is_isolated_temp_kdbx(db_path) {
        envelopes.push(config_dir.join("vault").join("automation-key.dpapi"));
        if let Some(h) = home_dir() {
            envelopes.push(
                h.join("KeePassXC-AppLauncher")
                    .join("store")
                    .join("automation-key.dpapi"),
            );
        }
    }
    let env_path = envelopes.into_iter().find(|p| p.is_file())?;
    let bytes = std::fs::read(&env_path).ok()?;
    let b64_env = encode_base64(&bytes);
    let script = concat!(
        "$ErrorActionPreference='Stop';",
        "Add-Type -AssemblyName System.Security;",
        "$b64=[Console]::In.ReadToEnd().Trim();",
        "$enc=[Convert]::FromBase64String($b64);",
        "$ent=[Text.Encoding]::UTF8.GetBytes('KeePassXC-AppLauncher scope v1');",
        "$raw=[System.Security.Cryptography.ProtectedData]::Unprotect($enc,$ent,'CurrentUser');",
        "[Convert]::ToBase64String($raw)"
    );
    let ps_root = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("WINDIR"))
        .ok();
    let ps = super::vault_store::windows_powershell_exe_from(ps_root.as_deref());
    let mut cmd = Command::new(ps);
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    let out = susi_paths::console::run_output_within(
        &mut cmd,
        Some(b64_env.as_bytes()),
        std::time::Duration::from_secs(10),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let pw = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if pw.is_empty() {
        None
    } else {
        Some(pw)
    }
}

#[cfg(windows)]
fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn resolve_kdbx_auth_for(
    config_dir: &Path,
    db_path: Option<&Path>,
    allow_interactive: bool,
) -> Result<KdbxAuthMaterial, String> {
    let key_file = discover_kdbx_key_file_for(config_dir, db_path);
    let mut passphrase = match resolve_unlock_secret(config_dir, PURPOSE_KDBX)? {
        Some((p, _)) if !p.trim().is_empty() => Some(p),
        _ => None,
    };
    #[cfg(windows)]
    if passphrase.is_none() {
        if let Some(db) = db_path {
            passphrase =
                resolve_windows_legacy_dpapi_passphrase(config_dir, db, key_file.is_some());
        }
    }
    let interactive_tty = allow_interactive
        && !susi_paths::console::is_unattended_env()
        && passphrase.is_none()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal();

    if passphrase.is_none() && key_file.is_none() && !interactive_tty {
        return Err(
            "KeePassXC unlock material is not enrolled for non-interactive use. Run `susi keys vault enroll --enroll-kdbx-passphrase` or configure a portable `.keyx`/`.key` sidecar, or set SUSI_VAULT_KDBX_PASSPHRASE / SUSI_COMMERCIAL_VAULT_KEY_FILE. Note: if Automation.kdbx uses Windows DPAPI (`automation-key.dpapi`), run `susi keys vault rekey` on Windows to convert it for CachyOS/Mint."
                .to_string(),
        );
    }
    Ok(KdbxAuthMaterial {
        passphrase,
        key_file,
        interactive_tty,
    })
}

struct KeepassCmdInvocation<'a> {
    subargs: &'a [&'a str],
    db_path: &'a Path,
    extra_args: &'a [&'a str],
}

fn build_keepassxc_command(
    spec: &KeepassCliSpec,
    inv: &KeepassCmdInvocation<'_>,
    auth: &KdbxAuthMaterial,
    headless_qt: bool,
) -> Command {
    let mut cmd = Command::new(&spec.program);
    if headless_qt {
        cmd.env("QT_QPA_PLATFORM", "offscreen")
            .env("QT_LOGGING_RULES", "qt.qpa.*=false");
    }
    if spec.is_flatpak {
        let mut insert_at = if spec.prefix_args.first().is_some_and(|a| a == "run") {
            1usize
        } else {
            0usize
        };
        cmd.args(&spec.prefix_args[..insert_at]);
        if headless_qt {
            cmd.arg("--env=QT_QPA_PLATFORM=offscreen")
                .arg("--env=QT_LOGGING_RULES=qt.qpa.*=false");
        }
        if let Some(parent) = inv.db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            cmd.arg(format!("--filesystem={}:ro", parent.display()));
        }
        if let Some(kf_parent) = auth
            .key_file
            .as_deref()
            .and_then(|p| p.parent())
            .filter(|p| !p.as_os_str().is_empty())
        {
            cmd.arg(format!("--filesystem={}:ro", kf_parent.display()));
        }
        let _ = &mut insert_at;
        cmd.args(&spec.prefix_args[insert_at..]);
    } else {
        cmd.args(&spec.prefix_args);
    }
    cmd.args(inv.subargs);
    if auth.passphrase.is_some() {
        cmd.arg("-q");
    } else if auth.key_file.is_some() && !auth.interactive_tty {
        cmd.arg("--no-password").arg("-q");
    }
    if let Some(kf) = &auth.key_file {
        cmd.arg("-k").arg(kf);
    }
    cmd.arg("--").arg(inv.db_path);
    cmd.args(inv.extra_args);
    cmd
}

fn run_keepassxc(
    cli: &str,
    subargs: &[&str],
    db_path: &Path,
    extra_args: &[&str],
    auth: &KdbxAuthMaterial,
) -> Result<String, String> {
    let spec = parse_keepassxc_cli_spec(cli);
    let bytes = run_keepassxc_bytes(&spec, subargs, db_path, extra_args, auth)?;
    String::from_utf8(bytes).map_err(|_| "keepassxc-cli emitted non-UTF-8 output".to_string())
}

fn run_keepassxc_bytes(
    spec: &KeepassCliSpec,
    subargs: &[&str],
    db_path: &Path,
    extra_args: &[&str],
    auth: &KdbxAuthMaterial,
) -> Result<Vec<u8>, String> {
    let headless_qt = is_headless_qt_env();
    let inv = KeepassCmdInvocation {
        subargs,
        db_path,
        extra_args,
    };
    let mut cmd = build_keepassxc_command(spec, &inv, auth, headless_qt);
    let display = &spec.display;

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
        res.map_err(|e| format!("failed to run `{display}`: {e}"))?
    } else if auth.interactive_tty {
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("failed to run `{display}`: {e}"))?
    } else {
        susi_paths::console::run_output_within(&mut cmd, None, KEEPASSXC_TIMEOUT)
            .map_err(|e| format!("failed to run `{display}`: {e}"))?
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {:?}", out.status.code())
        } else {
            stderr
        };
        return Err(format!(
            "`{display}` failed on {}: {detail}",
            db_path.display()
        ));
    }
    Ok(out.stdout)
}

/// List non-group entries in a KeePassXC database (`ls -R -f`).
pub fn list_kdbx_entries(
    config_dir: &Path,
    db_path: &Path,
    allow_interactive: bool,
) -> Result<Vec<String>, String> {
    validate_non_personal(db_path)?;
    let spec = resolve_keepassxc_cli_spec(config_dir);
    let auth = resolve_kdbx_auth_for(config_dir, Some(db_path), allow_interactive)?;
    let stdout_bytes = run_keepassxc_bytes(&spec, &["ls", "-R", "-f"], db_path, &[], &auth)?;
    let stdout = String::from_utf8(stdout_bytes)
        .map_err(|_| "keepassxc-cli emitted non-UTF-8 output".to_string())?;
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
    let spec = resolve_keepassxc_cli_spec(config_dir);
    let auth = resolve_kdbx_auth_for(config_dir, Some(db_path), allow_interactive)?;
    let stdout_bytes = run_keepassxc_bytes(
        &spec,
        &["show", "-s", "-a", attribute],
        db_path,
        &[clean_entry],
        &auth,
    )?;
    let stdout = String::from_utf8(stdout_bytes)
        .map_err(|_| "keepassxc-cli emitted non-UTF-8 output".to_string())?;
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
        "gpg signing" | "gpg_signing" | "git gpg signing" => {
            out.push(("signing", "GPG_SIGNING_PASSPHRASE".to_string()));
        }
        "ssh signing identity" | "ssh signing" | "git ssh signing" | "ssh_signing_identity" => {
            out.push(("signing", "SSH_SIGNING_PASSPHRASE".to_string()));
        }
        "openai" | "openai_api_key" => out.push(("cloud", "OPENAI_API_KEY".to_string())),
        "openai_org" | "openai_organization" => {
            out.push(("cloud", "OPENAI_ORGANIZATION".to_string()))
        }
        "anthropic" | "claude" | "anthropic_api_key" => {
            out.push(("cloud", "ANTHROPIC_API_KEY".to_string()))
        }
        "anthropic_workspace" | "anthropic_workspace_id" => {
            out.push(("cloud", "ANTHROPIC_WORKSPACE_ID".to_string()))
        }
        "gemini" | "google_gemini" | "gemini_api_key" => {
            out.push(("cloud", "GEMINI_API_KEY".to_string()))
        }
        "deepseek" | "deepseek_api_key" => out.push(("cloud", "DEEPSEEK_API_KEY".to_string())),
        "qwen" | "dashscope" | "dashscope_api_key" => {
            out.push(("cloud", "DASHSCOPE_API_KEY".to_string()))
        }
        "kimi" | "moonshot" | "moonshot_api_key" | "kimi_api_key" => {
            out.push(("cloud", "MOONSHOT_API_KEY".to_string()));
            out.push(("cloud", "KIMI_API_KEY".to_string()));
        }
        "glm" | "zhipu" | "zhipu_api_key" => out.push(("cloud", "ZHIPU_API_KEY".to_string())),
        "zai" | "z.ai" | "zai_api_key" => out.push(("cloud", "ZAI_API_KEY".to_string())),
        "siliconflow" | "siliconflow_api_key" => {
            out.push(("cloud", "SILICONFLOW_API_KEY".to_string()))
        }
        "litellm" | "litellm_master_key" => out.push(("cloud", "LITELLM_MASTER_KEY".to_string())),
        "openclaw" | "openclaw_gateway_token" => {
            out.push(("cloud", "OPENCLAW_GATEWAY_TOKEN".to_string()))
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

    let (src, dst) = if chosen == "pull" {
        (gdrive_path.as_path(), local_db)
    } else {
        (local_db, gdrive_path.as_path())
    };
    if dst.is_file() && has_valid_kdbx_magic(dst) && !has_valid_kdbx_magic(src) {
        return Err(format!(
            "refusing to overwrite valid KeePassXC database `{}` with non-KDBX/corrupt `{}`",
            dst.display(),
            src.display()
        ));
    }

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
    if dir_flag == "push" && !has_valid_kdbx_magic(local_db) {
        return Err(format!(
            "refusing rclone push of non-KDBX/corrupt `{}`",
            local_db.display()
        ));
    }
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
    let rclone_bin = resolve_rclone_cli();
    let mut rclone_cmd = Command::new(&rclone_bin);
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
    if !has_valid_kdbx_magic(&db_path) {
        return Err(format!(
            "KeePassXC database at `{}` is corrupt or truncated (missing KDBX magic header); check `susi keys vault doctor` or restore a valid `.bak.*` / Google Drive copy",
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
    let mut imported_signing = BTreeSet::new();

    let mut targets = VaultImportTargets {
        vault: &mut vault,
        cloud: &mut imported_cloud,
        commercial: &mut imported_commercial,
        signing: &mut imported_signing,
    };
    let spec = resolve_keepassxc_cli_spec(config_dir);
    let auth = resolve_kdbx_auth_for(config_dir, Some(&db_path), allow_interactive)?;
    let records = load_kdbx_entry_records(config_dir, &spec, &db_path, &auth, allow_interactive)?;
    let kdbx_ref = KeepassExecRef {
        spec: &spec,
        db_path: &db_path,
        auth: &auth,
    };
    for rec in records {
        let entry_path = rec.entry_path.clone();
        let attachment_names = rec.attachment_names.clone();
        import_kdbx_entry_record(&mut targets, rec);
        import_signing_attachments(&kdbx_ref, &entry_path, &attachment_names, &mut targets)?;
    }

    let total = vault.entry_count();
    if !dry_run {
        vault.save(config_dir)?;
        let mut cfg = VaultConfig::load(config_dir);
        cfg.vault_db_path = Some(db_path.clone());
        if let Some(kf) = discover_kdbx_key_file_for(config_dir, Some(&db_path)) {
            if cfg.key_file_path.is_none() {
                cfg.key_file_path = Some(kf);
            }
        }
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

struct KdbxEntryRecord {
    entry_path: String,
    username: String,
    password: String,
    notes: String,
    custom_fields: Vec<(String, String)>,
    attachment_names: Vec<String>,
}

impl Drop for KdbxEntryRecord {
    fn drop(&mut self) {
        super::vault_store::wipe_string(&mut self.password);
        super::vault_store::wipe_string(&mut self.notes);
        for (_, v) in &mut self.custom_fields {
            super::vault_store::wipe_string(v);
        }
    }
}

fn is_signing_gpg_leaf(norm_leaf: &str) -> bool {
    matches!(norm_leaf, "gpg signing" | "gpg_signing" | "git gpg signing")
}

fn is_signing_ssh_leaf(norm_leaf: &str) -> bool {
    matches!(
        norm_leaf,
        "ssh signing identity" | "ssh signing" | "git ssh signing" | "ssh_signing_identity"
    )
}

fn looks_like_hex_len(s: &str, expected_len: usize) -> bool {
    let t = s.trim();
    t.len() == expected_len && t.bytes().all(|b| b.is_ascii_hexdigit())
}

struct VaultImportTargets<'a> {
    vault: &'a mut SealedVault,
    cloud: &'a mut BTreeSet<String>,
    commercial: &'a mut BTreeSet<String>,
    signing: &'a mut BTreeSet<String>,
}

impl VaultImportTargets<'_> {
    fn insert(&mut self, scope: &str, key: &str, value: &str) {
        self.vault.set(scope, key, value);
        if scope == "cloud" {
            self.cloud.insert(key.to_string());
        } else if scope == "commercial" {
            self.commercial.insert(key.to_string());
        } else if scope == "signing" {
            self.signing.insert(key.to_string());
        }
    }
}

struct KeepassExecRef<'a> {
    spec: &'a KeepassCliSpec,
    db_path: &'a Path,
    auth: &'a KdbxAuthMaterial,
}

fn import_forward_kv(targets: &mut VaultImportTargets<'_>, raw_key: &str, raw_val: &str) {
    let k = raw_key.trim();
    let v = raw_val.trim();
    if k.is_empty() || v.is_empty() {
        return;
    }
    if k.starts_with("SUSI_COMMERCIAL") || k.starts_with("SUSI_CUSTOMER_LICENSE") {
        targets.insert("commercial", k, v);
    } else if k.starts_with("GPG_SIGNING_") || k.starts_with("SSH_SIGNING_") {
        targets.insert("signing", k, v);
    } else {
        targets.insert("cloud", k, v);
        if k == "GH_TOKEN" {
            targets.insert("cloud", "GITHUB_TOKEN", v);
        } else if k == "MOONSHOT_API_KEY" {
            targets.insert("cloud", "KIMI_API_KEY", v);
        } else if k == "ZHIPU_API_KEY" {
            targets.insert("cloud", "ZAI_API_KEY", v);
        }
    }
}

fn import_signing_kv(
    targets: &mut VaultImportTargets<'_>,
    norm_leaf: &str,
    raw_key: &str,
    raw_val: &str,
) {
    let k_upper = raw_key.trim().to_ascii_uppercase();
    let v = raw_val.trim();
    if k_upper.is_empty() || v.is_empty() {
        return;
    }
    let mapped = if is_signing_gpg_leaf(norm_leaf) {
        match k_upper.as_str() {
            "KEYGRIP" | "GPG_KEYGRIP" | "GPG_SIGNING_KEYGRIP" => Some("GPG_SIGNING_KEYGRIP"),
            "FINGERPRINT" | "GPG_FINGERPRINT" | "GPG_SIGNING_FINGERPRINT" => {
                Some("GPG_SIGNING_FINGERPRINT")
            }
            "KEY_ID" | "SIGNING_KEY" | "SIGNINGKEY" | "GPG_KEY_ID" | "GPG_SIGNING_KEY_ID" => {
                Some("GPG_SIGNING_KEY_ID")
            }
            "EMAIL" | "USER_EMAIL" | "GPG_SIGNING_EMAIL" => Some("GPG_SIGNING_EMAIL"),
            "GPG_SIGNING_PASSPHRASE" | "PASSPHRASE" => Some("GPG_SIGNING_PASSPHRASE"),
            _ => None,
        }
    } else if is_signing_ssh_leaf(norm_leaf) {
        match k_upper.as_str() {
            "FINGERPRINT" | "SSH_FINGERPRINT" | "SSH_SIGNING_FINGERPRINT" => {
                Some("SSH_SIGNING_FINGERPRINT")
            }
            "PUBLIC_KEY" | "SSH_PUBLIC_KEY" | "SSH_SIGNING_PUBLIC_KEY" => {
                Some("SSH_SIGNING_PUBLIC_KEY")
            }
            "SSH_SIGNING_PASSPHRASE" | "PASSPHRASE" => Some("SSH_SIGNING_PASSPHRASE"),
            _ => None,
        }
    } else {
        None
    };
    if let Some(target) = mapped {
        targets.insert("signing", target, v);
    }
}

fn import_kdbx_entry_record(targets: &mut VaultImportTargets<'_>, rec: KdbxEntryRecord) {
    let entry_path = rec.entry_path.as_str();
    let pw = rec.password.as_str();
    let notes = rec.notes.as_str();
    let leaf = entry_path.rsplit('/').next().unwrap_or(entry_path).trim();
    let norm_leaf = leaf.to_ascii_lowercase();

    if !pw.trim().is_empty() {
        for (scope, name) in map_kdbx_entry(entry_path) {
            targets.insert(scope, &name, pw.trim());
        }
    }

    if norm_leaf == "forward to apps" {
        for (fk, fv) in &rec.custom_fields {
            import_forward_kv(targets, fk, fv);
        }
        if !notes.trim().is_empty() {
            for (env_k, env_v) in parse_env_file(notes) {
                import_forward_kv(targets, &env_k, &env_v);
            }
        }
    } else if is_signing_gpg_leaf(&norm_leaf) {
        let user = rec.username.trim();
        if !user.is_empty() {
            let key_name = if looks_like_hex_len(user, 40) {
                "GPG_SIGNING_KEYGRIP"
            } else {
                "GPG_SIGNING_KEY_ID"
            };
            targets.insert("signing", key_name, user);
        }
        for (fk, fv) in &rec.custom_fields {
            import_signing_kv(targets, &norm_leaf, fk, fv);
        }
        for (nk, nv) in parse_env_file(notes) {
            import_signing_kv(targets, &norm_leaf, &nk, &nv);
        }
        if notes.contains("-----BEGIN PGP PRIVATE KEY BLOCK-----") {
            let mut normalized = notes.replace("\r\n", "\n");
            if !normalized.ends_with('\n') {
                normalized.push('\n');
            }
            targets.insert(
                "signing",
                "GPG_SIGNING_PRIVATE_KEY",
                normalized.trim_start(),
            );
            super::vault_store::wipe_string(&mut normalized);
        }
    } else if is_signing_ssh_leaf(&norm_leaf) {
        for (fk, fv) in &rec.custom_fields {
            import_signing_kv(targets, &norm_leaf, fk, fv);
        }
        for (nk, nv) in parse_env_file(notes) {
            import_signing_kv(targets, &norm_leaf, &nk, &nv);
        }
        for line in notes.lines() {
            let t = line.trim();
            if t.starts_with("ssh-ed25519 ")
                || t.starts_with("ssh-rsa ")
                || t.starts_with("ecdsa-sha2-")
            {
                targets.insert("signing", "SSH_SIGNING_PUBLIC_KEY", t);
            } else if t.starts_with("SHA256:") {
                targets.insert("signing", "SSH_SIGNING_FINGERPRINT", t);
            }
        }
    }
}

fn export_kdbx_attachment(
    spec: &KeepassCliSpec,
    db_path: &Path,
    auth: &KdbxAuthMaterial,
    entry_path: &str,
    attachment_name: &str,
) -> Result<Vec<u8>, String> {
    validate_non_personal(db_path)?;
    let clean_entry = entry_path.trim();
    let clean_att = attachment_name.trim();
    if clean_entry.is_empty()
        || clean_entry.starts_with('-')
        || clean_att.is_empty()
        || clean_att.starts_with('-')
    {
        return Err("invalid KeePassXC attachment reference".to_string());
    }
    match run_keepassxc_bytes(
        spec,
        &["attachment-export", "--stdout"],
        db_path,
        &[clean_entry, clean_att],
        auth,
    ) {
        Ok(bytes) => Ok(bytes),
        Err(stdout_err) => {
            // Fallback for older KeePassXC releases without `--stdout`.
            let tmp_dir = std::env::temp_dir().join(format!(
                "susi_kdbx_att_{}_{}",
                std::process::id(),
                now_unix()
            ));
            create_private_dir(&tmp_dir)
                .map_err(|e| format!("create attachment dir {}: {e}", tmp_dir.display()))?;
            let out_file = tmp_dir.join("payload.bin");
            let out_str = out_file.to_string_lossy().into_owned();
            let run_res = run_keepassxc_bytes(
                spec,
                &["attachment-export"],
                db_path,
                &[clean_entry, clean_att, &out_str],
                auth,
            );
            let read_res = match run_res {
                Ok(_) => std::fs::read(&out_file)
                    .map_err(|e| format!("read exported attachment `{clean_att}`: {e}")),
                Err(_) => Err(stdout_err),
            };
            if let Ok(mut zero) = std::fs::read(&out_file) {
                super::vault_store::wipe_bytes(&mut zero);
                let _ = std::fs::write(&out_file, []);
            }
            let _ = std::fs::remove_dir_all(&tmp_dir);
            read_res
        }
    }
}

fn import_signing_attachments(
    kdbx: &KeepassExecRef<'_>,
    entry_path: &str,
    attachment_names: &[String],
    targets: &mut VaultImportTargets<'_>,
) -> Result<(), String> {
    let leaf = entry_path.rsplit('/').next().unwrap_or(entry_path).trim();
    let norm_leaf = leaf.to_ascii_lowercase();
    if is_signing_ssh_leaf(&norm_leaf) {
        let mut names: Vec<String> = attachment_names.to_vec();
        if names.is_empty() {
            if let Ok(raw) = run_keepassxc_bytes(
                kdbx.spec,
                &["show", "-s", "--show-attachments"],
                kdbx.db_path,
                &[entry_path],
                kdbx.auth,
            ) {
                let text = String::from_utf8_lossy(&raw);
                names = parse_show_attachment_names(&text);
            }
        }
        let mut exported_priv = false;
        for name in names {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".pub") {
                if let Ok(mut bytes) =
                    export_kdbx_attachment(kdbx.spec, kdbx.db_path, kdbx.auth, entry_path, &name)
                {
                    if let Ok(text) = String::from_utf8(bytes.clone()) {
                        let norm = text.replace("\r\n", "\n").trim().to_string();
                        if !norm.is_empty() {
                            targets.insert("signing", "SSH_SIGNING_PUBLIC_KEY", &norm);
                        }
                    }
                    super::vault_store::wipe_bytes(&mut bytes);
                }
            } else if !exported_priv
                && (matches!(lower.as_str(), "id_ed25519" | "id_ecdsa" | "id_rsa")
                    || lower.starts_with("id_"))
            {
                let mut bytes =
                    export_kdbx_attachment(kdbx.spec, kdbx.db_path, kdbx.auth, entry_path, &name)?;
                if let Ok(mut text) = String::from_utf8(bytes.clone()) {
                    let mut norm = text.replace("\r\n", "\n");
                    if !norm.ends_with('\n') {
                        norm.push('\n');
                    }
                    if !norm.trim().is_empty() {
                        targets.insert("signing", "SSH_SIGNING_PRIVATE_KEY", &norm);
                        exported_priv = true;
                    }
                    super::vault_store::wipe_string(&mut norm);
                    super::vault_store::wipe_string(&mut text);
                }
                super::vault_store::wipe_bytes(&mut bytes);
            }
        }
    } else if is_signing_gpg_leaf(&norm_leaf) {
        for name in attachment_names {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".asc") || lower.contains("private") {
                if let Ok(mut bytes) =
                    export_kdbx_attachment(kdbx.spec, kdbx.db_path, kdbx.auth, entry_path, name)
                {
                    if let Ok(mut text) = String::from_utf8(bytes.clone()) {
                        let mut norm = text.replace("\r\n", "\n");
                        if norm.contains("-----BEGIN PGP PRIVATE KEY BLOCK-----") {
                            if !norm.ends_with('\n') {
                                norm.push('\n');
                            }
                            targets.insert("signing", "GPG_SIGNING_PRIVATE_KEY", &norm);
                        }
                        super::vault_store::wipe_string(&mut norm);
                        super::vault_store::wipe_string(&mut text);
                    }
                    super::vault_store::wipe_bytes(&mut bytes);
                }
            }
        }
    }
    Ok(())
}

fn parse_show_attachment_names(show_output: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_att = false;
    for line in show_output.lines() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("Attachments:") {
            in_att = true;
            continue;
        }
        if !in_att {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        if let Some(idx) = trimmed.rfind(" (") {
            let name = trimmed[..idx].trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        } else {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn load_kdbx_entry_records(
    config_dir: &Path,
    spec: &KeepassCliSpec,
    db_path: &Path,
    auth: &KdbxAuthMaterial,
    allow_interactive: bool,
) -> Result<Vec<KdbxEntryRecord>, String> {
    if let Ok(xml_bytes) = run_keepassxc_bytes(spec, &["export", "-f", "xml"], db_path, &[], auth) {
        if let Ok(mut xml_text) = String::from_utf8(xml_bytes) {
            if xml_text.contains("<KeePassFile") {
                let entries = parse_kdbx_xml_entries(&xml_text);
                super::vault_store::wipe_string(&mut xml_text);
                return Ok(entries);
            }
            super::vault_store::wipe_string(&mut xml_text);
        }
    }
    if let Ok(csv_records) = export_kdbx_csv_records_with(spec, db_path, auth) {
        if !csv_records.is_empty() {
            return Ok(csv_records);
        }
    }
    let _ = allow_interactive;
    let entries = list_kdbx_entries(config_dir, db_path, false)?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let leaf = entry.rsplit('/').next().unwrap_or(&entry).trim();
        let norm_leaf = leaf.to_ascii_lowercase();
        let password = String::from_utf8(run_keepassxc_bytes(
            spec,
            &["show", "-s", "-a", "Password"],
            db_path,
            &[&entry],
            auth,
        )?)
        .unwrap_or_default()
        .trim_end_matches(&['\r', '\n'][..])
        .to_string();
        let needs_extra = norm_leaf == "forward to apps"
            || is_signing_gpg_leaf(&norm_leaf)
            || is_signing_ssh_leaf(&norm_leaf);
        let notes = if needs_extra {
            run_keepassxc_bytes(
                spec,
                &["show", "-s", "-a", "Notes"],
                db_path,
                &[&entry],
                auth,
            )
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
        } else {
            String::new()
        };
        let username = if is_signing_gpg_leaf(&norm_leaf) {
            run_keepassxc_bytes(
                spec,
                &["show", "-s", "-a", "UserName"],
                db_path,
                &[&entry],
                auth,
            )
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
            .trim()
            .to_string()
        } else {
            String::new()
        };
        out.push(KdbxEntryRecord {
            entry_path: entry,
            username,
            password,
            notes,
            custom_fields: Vec::new(),
            attachment_names: Vec::new(),
        });
    }
    Ok(out)
}

fn unescape_xml(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let entity = &after[..semi];
            let replaced = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(ch) = replaced {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn parse_kdbx_xml_entries(xml: &str) -> Vec<KdbxEntryRecord> {
    let mut records = Vec::new();
    let mut group_stack: Vec<String> = Vec::new();
    let mut entry_depth: usize = 0;
    let mut history_depth: usize = 0;
    let mut deleted_depth: usize = 0;
    let mut in_group_name = false;
    let mut in_string = false;
    let mut in_binary = false;
    let mut in_key = false;
    let mut in_value = false;

    let mut cur_key = String::new();
    let mut cur_val = String::new();
    let mut cur_title = String::new();
    let mut cur_user = String::new();
    let mut cur_pass = String::new();
    let mut cur_notes = String::new();
    let mut cur_custom: Vec<(String, String)> = Vec::new();
    let mut cur_att: Vec<String> = Vec::new();

    let mut pos = 0usize;
    let bytes = xml.as_bytes();
    while pos < bytes.len() {
        let Some(lt_rel) = xml[pos..].find('<') else {
            break;
        };
        if lt_rel > 0 {
            let text_raw = &xml[pos..pos + lt_rel];
            if in_group_name && !text_raw.is_empty() {
                if let Some(last) = group_stack.last_mut() {
                    last.push_str(&unescape_xml(text_raw));
                }
            } else if entry_depth == 1 && history_depth == 0 && deleted_depth == 0 {
                if in_key {
                    cur_key.push_str(&unescape_xml(text_raw));
                } else if in_value {
                    cur_val.push_str(&unescape_xml(text_raw));
                }
            }
        }
        let tag_start = pos + lt_rel;
        if xml[tag_start..].starts_with("<!--") {
            if let Some(end) = xml[tag_start + 4..].find("-->") {
                pos = tag_start + 4 + end + 3;
                continue;
            }
            break;
        }
        if xml[tag_start..].starts_with("<![CDATA[") {
            if let Some(end) = xml[tag_start + 9..].find("]]>") {
                let cdata = &xml[tag_start + 9..tag_start + 9 + end];
                if entry_depth == 1 && history_depth == 0 && deleted_depth == 0 {
                    if in_key {
                        cur_key.push_str(cdata);
                    } else if in_value {
                        cur_val.push_str(cdata);
                    }
                }
                pos = tag_start + 9 + end + 3;
                continue;
            }
            break;
        }
        if xml[tag_start..].starts_with("<?") {
            if let Some(end) = xml[tag_start + 2..].find("?>") {
                pos = tag_start + 2 + end + 2;
                continue;
            }
            break;
        }
        let Some(gt_rel) = xml[tag_start + 1..].find('>') else {
            break;
        };
        let inside = xml[tag_start + 1..tag_start + 1 + gt_rel].trim();
        pos = tag_start + 1 + gt_rel + 1;

        if let Some(end_name_raw) = inside.strip_prefix('/') {
            let tag = end_name_raw.trim();
            match tag {
                "Group" => {
                    group_stack.pop();
                }
                "Name" => in_group_name = false,
                "History" => history_depth = history_depth.saturating_sub(1),
                "DeletedObjects" => deleted_depth = deleted_depth.saturating_sub(1),
                "Key" => {
                    if in_binary && entry_depth == 1 && history_depth == 0 && deleted_depth == 0 {
                        let name = cur_key.trim().to_string();
                        if !name.is_empty() {
                            cur_att.push(name);
                        }
                        cur_key.clear();
                    }
                    in_key = false;
                }
                "Value" => in_value = false,
                "String" => {
                    if in_string && entry_depth == 1 && history_depth == 0 && deleted_depth == 0 {
                        let k = std::mem::take(&mut cur_key);
                        let v = std::mem::take(&mut cur_val);
                        match k.trim() {
                            "Title" => cur_title = v,
                            "UserName" => cur_user = v,
                            "Password" => cur_pass = v,
                            "Notes" => cur_notes = v,
                            "URL" | "" => {}
                            other => cur_custom.push((other.to_string(), v)),
                        }
                    }
                    in_string = false;
                }
                "Binary" => in_binary = false,
                "Entry" => {
                    if entry_depth == 1 && history_depth == 0 && deleted_depth == 0 {
                        let in_recycle_bin = group_stack
                            .iter()
                            .any(|g| g.trim().eq_ignore_ascii_case("recycle bin"));
                        let title = std::mem::take(&mut cur_title);
                        let trimmed_title = title.trim();
                        if !in_recycle_bin && !trimmed_title.is_empty() {
                            let mut parts: Vec<&str> = group_stack
                                .iter()
                                .skip(1)
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty())
                                .collect();
                            parts.push(trimmed_title);
                            records.push(KdbxEntryRecord {
                                entry_path: parts.join("/"),
                                username: std::mem::take(&mut cur_user),
                                password: std::mem::take(&mut cur_pass),
                                notes: std::mem::take(&mut cur_notes),
                                custom_fields: std::mem::take(&mut cur_custom),
                                attachment_names: std::mem::take(&mut cur_att),
                            });
                        } else {
                            super::vault_store::wipe_string(&mut cur_pass);
                            super::vault_store::wipe_string(&mut cur_notes);
                            cur_user.clear();
                            cur_custom.clear();
                            cur_att.clear();
                        }
                    }
                    entry_depth = entry_depth.saturating_sub(1);
                }
                _ => {}
            }
            continue;
        }

        let self_closing = inside.ends_with('/');
        let clean = inside.strip_suffix('/').unwrap_or(inside).trim();
        let tag_name = clean.split_whitespace().next().unwrap_or("");
        match tag_name {
            "Group" if !self_closing => group_stack.push(String::new()),
            "Name" if !self_closing && entry_depth == 0 && deleted_depth == 0 => {
                in_group_name = true;
            }
            "History" if !self_closing => history_depth += 1,
            "DeletedObjects" if !self_closing => deleted_depth += 1,
            "Entry" if !self_closing => {
                entry_depth += 1;
                if entry_depth == 1 && history_depth == 0 && deleted_depth == 0 {
                    cur_title.clear();
                    cur_user.clear();
                    cur_pass.clear();
                    cur_notes.clear();
                    cur_custom.clear();
                    cur_att.clear();
                }
            }
            "String"
                if !self_closing
                    && entry_depth == 1
                    && history_depth == 0
                    && deleted_depth == 0 =>
            {
                in_string = true;
                cur_key.clear();
                cur_val.clear();
            }
            "Binary"
                if !self_closing
                    && entry_depth == 1
                    && history_depth == 0
                    && deleted_depth == 0 =>
            {
                in_binary = true;
                cur_key.clear();
            }
            "Key" if !self_closing && (in_string || in_binary) => {
                in_key = true;
                cur_key.clear();
            }
            "Value" if !self_closing && in_string => {
                in_value = true;
                cur_val.clear();
            }
            _ => {}
        }
    }
    records
}

fn export_kdbx_csv_records_with(
    spec: &KeepassCliSpec,
    db_path: &Path,
    auth: &KdbxAuthMaterial,
) -> Result<Vec<KdbxEntryRecord>, String> {
    validate_non_personal(db_path)?;
    let bytes = run_keepassxc_bytes(spec, &["export", "-f", "csv"], db_path, &[], auth)?;
    let mut csv_text =
        String::from_utf8(bytes).map_err(|_| "keepassxc-cli emitted non-UTF-8 CSV".to_string())?;
    let rows = parse_csv_rows(&csv_text);
    drop_secret_string(std::mem::take(&mut csv_text));
    let Some(header) = rows.first() else {
        return Ok(Vec::new());
    };
    let col = |name: &str| header.iter().position(|h| h.eq_ignore_ascii_case(name));
    let group_idx = col("Group");
    let title_idx = col("Title").ok_or_else(|| "CSV missing Title column".to_string())?;
    let user_idx = col("Username").or_else(|| col("UserName"));
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
        let username = user_idx
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_default();
        let password = row.get(pass_idx).cloned().unwrap_or_default();
        let notes = notes_idx
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_default();
        out.push(KdbxEntryRecord {
            entry_path,
            username,
            password,
            notes,
            custom_fields: Vec::new(),
            attachment_names: Vec::new(),
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

/// Resolve client-scoped environment pairs from `SealedVault` (or cloud overlay fallback) without global shell export.
pub fn resolve_client_env(
    config_dir: &Path,
    client: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    let _ = auto_sync_if_enrolled(config_dir);
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct SigningPrimeReport {
    pub gpg_primed_keygrips: Vec<String>,
    pub ssh_primed: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GitSetupReport {
    pub wrapper_path: PathBuf,
    pub gpg_program_set: String,
    pub ssh_command_action: String,
    pub signing_key_set: Option<String>,
    pub commit_gpgsign_enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VaultDoctorReport {
    pub os: String,
    pub config_dir: PathBuf,
    pub keepassxc_cli: String,
    pub keepassxc_available: bool,
    pub keepassxc_version: Option<String>,
    pub kdbx_path: Option<PathBuf>,
    pub kdbx_valid_magic: bool,
    pub kdbx_key_file: Option<PathBuf>,
    pub legacy_dpapi_present: bool,
    pub dbus_bus: Option<String>,
    pub headless_qt_needed: bool,
    pub real_gpg_path: Option<String>,
    pub gpg_preset_passphrase_path: Option<String>,
    pub gpg_agent_allow_preset: bool,
    pub gpg_keygrips: Vec<String>,
    pub git_gpg_program: Option<String>,
    pub git_ssh_command: Option<String>,
    pub git_signing_key: Option<String>,
    pub ssh_auth_sock: Option<String>,
    pub ssh_add_path: Option<String>,
    pub vault_has_gpg_passphrase: bool,
    pub vault_has_ssh_identity: bool,
    pub issues: Vec<String>,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RekeyReport {
    pub db_path: PathBuf,
    pub backup_path: PathBuf,
    pub key_file: Option<PathBuf>,
    pub has_passphrase: bool,
    pub kdbx_unlock_backend: Option<String>,
}

fn is_susi_wrapper_binary(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    matches!(
        stem,
        "susi" | "susi-gpg" | "susi-git-gpg" | "susi-gpg-program" | "git-gpg-agent"
    )
}

fn is_windows_only_git_path(val: &str) -> bool {
    let lower = val.trim().to_ascii_lowercase();
    lower.contains(".exe")
        || lower.contains(":\\")
        || lower.contains(":/")
        || lower.contains("windows/system32/openssh")
        || lower.contains("windows\\system32\\openssh")
        || lower.contains("keepassxc-applauncher")
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
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in exe_names {
                let candidate = dir.join(name);
                if candidate.is_file() && (!skip_wrappers || !is_susi_wrapper_binary(&candidate)) {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
    }
    for cand in extra_candidates {
        if cand.is_file() && (!skip_wrappers || !is_susi_wrapper_binary(cand)) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

fn resolve_real_gpg_binary() -> Option<String> {
    let mut extras = Vec::new();
    #[cfg(windows)]
    {
        extras.push(PathBuf::from(r"C:\Program Files\GnuPG\bin\gpg.exe"));
        extras.push(PathBuf::from(r"C:\Program Files (x86)\GnuPG\bin\gpg.exe"));
        extras.push(PathBuf::from(r"C:\Program Files\Git\usr\bin\gpg.exe"));
        if let Some(up) = home_dir() {
            extras.push(
                up.join("scoop")
                    .join("apps")
                    .join("gpg")
                    .join("current")
                    .join("bin")
                    .join("gpg.exe"),
            );
        }
        resolve_non_wrapper_executable(
            non_empty_env("SUSI_REAL_GPG").as_deref(),
            &["gpg.exe", "gpg2.exe"],
            &extras,
            true,
        )
    }
    #[cfg(target_os = "macos")]
    {
        extras.extend([
            PathBuf::from("/opt/homebrew/bin/gpg"),
            PathBuf::from("/usr/local/bin/gpg"),
            PathBuf::from("/usr/local/MacGPG2/bin/gpg"),
        ]);
        resolve_non_wrapper_executable(
            non_empty_env("SUSI_REAL_GPG").as_deref(),
            &["gpg", "gpg2"],
            &extras,
            true,
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        extras.extend([
            PathBuf::from("/usr/bin/gpg"),
            PathBuf::from("/usr/bin/gpg2"),
            PathBuf::from("/usr/local/bin/gpg"),
        ]);
        resolve_non_wrapper_executable(
            non_empty_env("SUSI_REAL_GPG").as_deref(),
            &["gpg", "gpg2"],
            &extras,
            true,
        )
    }
}

fn resolve_gpgconf_binary(real_gpg: Option<&str>) -> Option<String> {
    let mut extras = Vec::new();
    if let Some(gpg) = real_gpg {
        if let Some(parent) = Path::new(gpg).parent() {
            #[cfg(windows)]
            extras.push(parent.join("gpgconf.exe"));
            #[cfg(not(windows))]
            extras.push(parent.join("gpgconf"));
        }
    }
    #[cfg(windows)]
    {
        extras.push(PathBuf::from(r"C:\Program Files\GnuPG\bin\gpgconf.exe"));
        extras.push(PathBuf::from(
            r"C:\Program Files (x86)\GnuPG\bin\gpgconf.exe",
        ));
        resolve_non_wrapper_executable(None, &["gpgconf.exe"], &extras, false)
    }
    #[cfg(not(windows))]
    {
        extras.extend([
            PathBuf::from("/usr/bin/gpgconf"),
            PathBuf::from("/opt/homebrew/bin/gpgconf"),
            PathBuf::from("/usr/local/bin/gpgconf"),
        ]);
        resolve_non_wrapper_executable(None, &["gpgconf"], &extras, false)
    }
}

fn gpg_preset_passphrase_candidates_with(real_gpg: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(gpg) = real_gpg {
        if let Some(parent) = Path::new(gpg).parent() {
            #[cfg(windows)]
            out.push(parent.join("gpg-preset-passphrase.exe"));
            #[cfg(not(windows))]
            {
                out.push(parent.join("gpg-preset-passphrase"));
                if let Some(prefix) = parent.parent() {
                    out.push(
                        prefix
                            .join("lib")
                            .join("gnupg")
                            .join("gpg-preset-passphrase"),
                    );
                    out.push(prefix.join("libexec").join("gpg-preset-passphrase"));
                }
            }
        }
    }
    #[cfg(windows)]
    {
        out.push(PathBuf::from(
            r"C:\Program Files\GnuPG\bin\gpg-preset-passphrase.exe",
        ));
        out.push(PathBuf::from(
            r"C:\Program Files (x86)\GnuPG\bin\gpg-preset-passphrase.exe",
        ));
    }
    #[cfg(not(windows))]
    {
        out.extend([
            PathBuf::from("/usr/lib/gnupg/gpg-preset-passphrase"),
            PathBuf::from("/usr/libexec/gpg-preset-passphrase"),
            PathBuf::from("/usr/lib/gnupg2/gpg-preset-passphrase"),
            PathBuf::from("/opt/homebrew/opt/gnupg/libexec/gpg-preset-passphrase"),
            PathBuf::from("/usr/local/opt/gnupg/libexec/gpg-preset-passphrase"),
            PathBuf::from("/usr/local/MacGPG2/libexec/gpg-preset-passphrase"),
        ]);
    }
    out
}

fn resolve_gpg_preset_passphrase_binary(real_gpg: Option<&str>) -> Option<String> {
    let extras = gpg_preset_passphrase_candidates_with(real_gpg);
    #[cfg(windows)]
    let names = &["gpg-preset-passphrase.exe"][..];
    #[cfg(not(windows))]
    let names = &["gpg-preset-passphrase"][..];
    resolve_non_wrapper_executable(None, names, &extras, false)
}

fn resolve_gpg_connect_agent_binary(real_gpg: Option<&str>) -> Option<String> {
    let mut extras = Vec::new();
    if let Some(gpg) = real_gpg {
        if let Some(parent) = Path::new(gpg).parent() {
            #[cfg(windows)]
            extras.push(parent.join("gpg-connect-agent.exe"));
            #[cfg(not(windows))]
            extras.push(parent.join("gpg-connect-agent"));
        }
    }
    #[cfg(windows)]
    {
        extras.push(PathBuf::from(
            r"C:\Program Files\GnuPG\bin\gpg-connect-agent.exe",
        ));
        resolve_non_wrapper_executable(None, &["gpg-connect-agent.exe"], &extras, false)
    }
    #[cfg(not(windows))]
    {
        extras.extend([
            PathBuf::from("/usr/bin/gpg-connect-agent"),
            PathBuf::from("/opt/homebrew/bin/gpg-connect-agent"),
            PathBuf::from("/usr/local/bin/gpg-connect-agent"),
        ]);
        resolve_non_wrapper_executable(None, &["gpg-connect-agent"], &extras, false)
    }
}

fn resolve_ssh_add_binary() -> Option<String> {
    #[cfg(windows)]
    {
        let extras = [
            PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh-add.exe"),
            PathBuf::from(r"C:\Program Files\Git\usr\bin\ssh-add.exe"),
        ];
        resolve_non_wrapper_executable(None, &["ssh-add.exe"], &extras, false)
    }
    #[cfg(not(windows))]
    {
        let extras = [
            PathBuf::from("/usr/bin/ssh-add"),
            PathBuf::from("/usr/local/bin/ssh-add"),
        ];
        resolve_non_wrapper_executable(None, &["ssh-add"], &extras, false)
    }
}

fn resolve_gnupg_homedir(gpgconf: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = non_empty_env("GNUPGHOME") {
        return Some(PathBuf::from(dir));
    }
    if let Some(bin) = gpgconf {
        let mut cmd = Command::new(bin);
        cmd.args(["--list-dirs", "homedir"]);
        if let Ok(out) = susi_paths::console::run_output_within(
            &mut cmd,
            None,
            std::time::Duration::from_secs(5),
        ) {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !text.is_empty() {
                    return Some(PathBuf::from(text));
                }
            }
        }
    }
    home_dir().map(|h| h.join(".gnupg"))
}

fn ensure_gpg_agent_preset_allowed(gpgconf: Option<&str>) -> Result<bool, String> {
    let Some(homedir) = resolve_gnupg_homedir(gpgconf) else {
        return Ok(false);
    };
    create_private_dir(&homedir).map_err(|e| format!("create gnupg dir: {e}"))?;
    let conf_path = homedir.join("gpg-agent.conf");
    let existing = std::fs::read_to_string(&conf_path).unwrap_or_default();
    let already = existing.lines().any(|line| {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        trimmed == "allow-preset-passphrase"
    });
    if !already {
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str("allow-preset-passphrase\n");
        super::json_util::atomic_write_bytes(&conf_path, next.as_bytes())
            .map_err(|e| format!("write {}: {e}", conf_path.display()))?;
    }
    if let Some(bin) = gpgconf {
        let mut launch = Command::new(bin);
        launch.args(["--launch", "gpg-agent"]);
        let _ = susi_paths::console::run_output_within(
            &mut launch,
            None,
            std::time::Duration::from_secs(5),
        );
        if !already {
            let mut reload = Command::new(bin);
            reload.args(["--reload", "gpg-agent"]);
            let _ = susi_paths::console::run_output_within(
                &mut reload,
                None,
                std::time::Duration::from_secs(5),
            );
        }
    }
    Ok(true)
}

fn parse_gpg_colons_keygrips(colons: &str, filter_hint: Option<&str>) -> Vec<String> {
    let normalized_hint = filter_hint
        .map(|s| {
            s.trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X")
                .trim()
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_uppercase());

    #[derive(Default)]
    struct Block {
        key_id: String,
        fpr: String,
        caps: String,
        grp: String,
        is_primary: bool,
    }

    let mut primary_matches = false;
    let mut blocks: Vec<Block> = Vec::new();
    let mut current: Option<Block> = None;

    for line in colons.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let rec = fields.first().copied().unwrap_or("");
        match rec {
            "sec" | "ssb" => {
                if let Some(b) = current.take() {
                    blocks.push(b);
                }
                let is_primary = rec == "sec";
                if is_primary {
                    primary_matches = false;
                }
                let key_id = fields
                    .get(4)
                    .copied()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_uppercase();
                let caps = fields.get(11).copied().unwrap_or("").trim().to_string();
                if let Some(hint) = &normalized_hint {
                    if is_primary && (key_id == *hint || key_id.ends_with(hint.as_str())) {
                        primary_matches = true;
                    }
                }
                current = Some(Block {
                    key_id,
                    fpr: String::new(),
                    caps,
                    grp: String::new(),
                    is_primary,
                });
            }
            "fpr" => {
                if let Some(b) = current.as_mut() {
                    let fpr = fields
                        .get(9)
                        .copied()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_uppercase();
                    if !fpr.is_empty() {
                        b.fpr = fpr.clone();
                        if let Some(hint) = &normalized_hint {
                            if b.is_primary && (fpr == *hint || fpr.ends_with(hint.as_str())) {
                                primary_matches = true;
                            }
                        }
                    }
                }
            }
            "grp" => {
                if let Some(b) = current.as_mut() {
                    let grp = fields
                        .get(9)
                        .copied()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_uppercase();
                    if !grp.is_empty() {
                        b.grp = grp;
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(b) = current {
        blocks.push(b);
    }

    fn push_unique(out: &mut Vec<String>, g: &str) {
        if !g.is_empty() && !out.iter().any(|existing| existing == g) {
            out.push(g.to_string());
        }
    }

    let mut out = Vec::new();
    if let Some(hint) = &normalized_hint {
        // First collect keygrips for blocks matching the signing key hint (either direct key/subkey or primary key).
        let mut active_primary_match = false;
        for b in &blocks {
            let direct = b.key_id == *hint
                || b.key_id.ends_with(hint.as_str())
                || b.fpr == *hint
                || b.fpr.ends_with(hint.as_str())
                || b.grp == *hint;
            if b.is_primary {
                active_primary_match = direct;
            }
            if (direct || active_primary_match) && !b.grp.is_empty() {
                push_unique(&mut out, &b.grp);
            }
        }
        let _ = primary_matches;
        if !out.is_empty() {
            return out;
        }
    }

    // Signing-capable secret keys/subkeys first, then any remaining secret keygrips.
    for b in &blocks {
        if (b.caps.contains('s') || b.caps.contains('S') || b.caps.contains('c'))
            && !b.grp.is_empty()
        {
            push_unique(&mut out, &b.grp);
        }
    }
    for b in &blocks {
        if !b.grp.is_empty() {
            push_unique(&mut out, &b.grp);
        }
    }
    out
}

fn discover_gpg_keygrips(real_gpg: &str, filter_hint: Option<&str>) -> Vec<String> {
    let mut cmd = Command::new(real_gpg);
    cmd.args(["--with-colons", "--with-keygrip", "--list-secret-keys"]);
    if let Some(hint) = filter_hint.map(str::trim).filter(|s| !s.is_empty()) {
        let mut exact_cmd = Command::new(real_gpg);
        exact_cmd.args([
            "--with-colons",
            "--with-keygrip",
            "--list-secret-keys",
            hint,
        ]);
        if let Ok(out) = susi_paths::console::run_output_within(
            &mut exact_cmd,
            None,
            std::time::Duration::from_secs(5),
        ) {
            if out.status.success() {
                let parsed =
                    parse_gpg_colons_keygrips(&String::from_utf8_lossy(&out.stdout), Some(hint));
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }
    }
    if let Ok(out) =
        susi_paths::console::run_output_within(&mut cmd, None, std::time::Duration::from_secs(5))
    {
        if out.status.success() {
            return parse_gpg_colons_keygrips(&String::from_utf8_lossy(&out.stdout), filter_hint);
        }
    }
    Vec::new()
}

fn git_global_get(key: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(["config", "--global", "--get", key]);
    let out =
        susi_paths::console::run_output_within(&mut cmd, None, std::time::Duration::from_secs(5))
            .ok()?;
    if !out.status.success() {
        return None;
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!val.is_empty()).then_some(val)
}

fn encode_hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

fn preset_gpg_passphrase(
    gpg_pass: &str,
    real_gpg: Option<&str>,
    gpgconf: Option<&str>,
    keygrips: &[String],
) -> Result<Vec<String>, String> {
    if keygrips.is_empty() {
        return Err("no GPG signing keygrips found in local keyring or vault".to_string());
    }
    ensure_gpg_agent_preset_allowed(gpgconf)?;
    let preset_bin = resolve_gpg_preset_passphrase_binary(real_gpg);
    let connect_bin = resolve_gpg_connect_agent_binary(real_gpg);
    if preset_bin.is_none() && connect_bin.is_none() {
        return Err(
            "neither gpg-preset-passphrase nor gpg-connect-agent was found on this system"
                .to_string(),
        );
    }

    let mut primed = Vec::new();
    let mut pass_line = String::with_capacity(gpg_pass.len() + 1);
    pass_line.push_str(gpg_pass);
    pass_line.push('\n');

    for grip in keygrips {
        let mut ok_this = false;
        if let Some(bin) = &preset_bin {
            let mut cmd = Command::new(bin);
            cmd.args(["--preset", grip]);
            if let Ok(out) = susi_paths::console::run_output_within(
                &mut cmd,
                Some(pass_line.as_bytes()),
                std::time::Duration::from_secs(5),
            ) {
                if out.status.success() {
                    ok_this = true;
                }
            }
        }
        if !ok_this {
            if let Some(conn) = &connect_bin {
                let mut hex_pass = encode_hex_upper(gpg_pass.as_bytes());
                let mut assuan = format!("PRESET_PASSPHRASE {grip} -1 {hex_pass}\n/bye\n");
                let mut cmd = Command::new(conn);
                let res = susi_paths::console::run_output_within(
                    &mut cmd,
                    Some(assuan.as_bytes()),
                    std::time::Duration::from_secs(5),
                );
                super::vault_store::wipe_string(&mut hex_pass);
                super::vault_store::wipe_string(&mut assuan);
                if let Ok(out) = res {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if out.status.success() && !stdout.contains("ERR ") {
                        ok_this = true;
                    }
                }
            }
        }
        if ok_this {
            primed.push(grip.clone());
        }
    }
    super::vault_store::wipe_string(&mut pass_line);
    if primed.is_empty() {
        return Err("gpg-agent rejected passphrase preset for all signing keygrips".to_string());
    }
    Ok(primed)
}

fn resolve_ssh_auth_sock_with<F>(
    env_sock: Option<&str>,
    xdg_runtime: Option<&str>,
    socket_exists: F,
) -> Option<String>
where
    F: Fn(&Path) -> bool,
{
    if let Some(sock) = env_sock.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(sock.to_string());
    }
    if let Some(runtime) = xdg_runtime.map(str::trim).filter(|s| !s.is_empty()) {
        let base = runtime.trim_end_matches(&['/', '\\'][..]);
        let sep = if base.contains('\\') && !base.contains('/') {
            '\\'
        } else {
            '/'
        };
        for rel in [
            "ssh-agent.socket",
            "gcr/ssh",
            "keyring/ssh",
            "openssh_agent",
        ] {
            let joined = format!("{base}{sep}{rel}");
            if socket_exists(Path::new(&joined)) {
                return Some(joined);
            }
        }
    }
    None
}

fn resolve_ssh_auth_sock() -> Option<String> {
    let env_sock = std::env::var("SSH_AUTH_SOCK").ok();
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
    resolve_ssh_auth_sock_with(env_sock.as_deref(), xdg.as_deref(), |p| p.exists())
}

fn prime_ssh_signing_identity(
    private_key_pem: &str,
    passphrase: Option<&str>,
    expected_fp: Option<&str>,
    expected_pub: Option<&str>,
) -> Result<bool, String> {
    let Some(ssh_add) = resolve_ssh_add_binary() else {
        return Err("ssh-add binary not found".to_string());
    };
    let sock = resolve_ssh_auth_sock();
    if sock.is_some() || cfg!(windows) {
        let mut check_cmd = Command::new(&ssh_add);
        if let Some(ref s) = sock {
            check_cmd.env("SSH_AUTH_SOCK", s);
        }
        check_cmd.arg("-l");
        if let Ok(out) = susi_paths::console::run_output_within(
            &mut check_cmd,
            None,
            std::time::Duration::from_secs(5),
        ) {
            if out.status.success() {
                let list = String::from_utf8_lossy(&out.stdout);
                if let Some(fp) = expected_fp.map(str::trim).filter(|s| !s.is_empty()) {
                    if list.contains(fp) {
                        return Ok(true);
                    }
                }
                if let Some(pub_line) = expected_pub.map(str::trim).filter(|s| !s.is_empty()) {
                    let mut pub_cmd = Command::new(&ssh_add);
                    if let Some(ref s) = sock {
                        pub_cmd.env("SSH_AUTH_SOCK", s);
                    }
                    pub_cmd.arg("-L");
                    if let Ok(pub_out) = susi_paths::console::run_output_within(
                        &mut pub_cmd,
                        None,
                        std::time::Duration::from_secs(5),
                    ) {
                        let key_blob = pub_line.split_whitespace().nth(1).unwrap_or("");
                        if !key_blob.is_empty()
                            && String::from_utf8_lossy(&pub_out.stdout).contains(key_blob)
                        {
                            return Ok(true);
                        }
                    }
                }
            }
        }
    }

    // Write temporary private key inside 0700 dir with strict cleanup after ssh-add.
    let tmp_dir = std::env::temp_dir().join(format!(
        "susi_ssh_prime_{}_{}",
        std::process::id(),
        now_unix()
    ));
    create_private_dir(&tmp_dir).map_err(|e| format!("create private ssh staging dir: {e}"))?;
    let key_file = tmp_dir.join("id_ed25519");
    let mut normalized = private_key_pem.replace("\r\n", "\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    let write_res = super::vault_store::write_private_vault_bytes(&key_file, normalized.as_bytes())
        .map_err(|e| format!("write staging ssh key: {e}"));
    super::vault_store::wipe_string(&mut normalized);
    write_res?;

    // If passphrase-protected SSH key, feed via minimal askpass helper script if needed.
    let askpass_file = if let Some(pass) = passphrase.filter(|s| !s.trim().is_empty()) {
        #[cfg(windows)]
        let script_path = tmp_dir.join("askpass.cmd");
        #[cfg(not(windows))]
        let script_path = tmp_dir.join("askpass.sh");
        #[cfg(windows)]
        let script_body = format!("@echo off\r\necho {pass}\r\n");
        #[cfg(not(windows))]
        let script_body = {
            let escaped = pass.replace('\'', "'\\''");
            format!("#!/bin/sh\nprintf '%s\\n' '{escaped}'\n")
        };
        let mut body_mut = script_body;
        let _ = super::vault_store::write_private_vault_bytes(&script_path, body_mut.as_bytes());
        super::vault_store::wipe_string(&mut body_mut);
        Some(script_path)
    } else {
        None
    };

    let mut add_cmd = Command::new(&ssh_add);
    if let Some(ref s) = sock {
        add_cmd.env("SSH_AUTH_SOCK", s);
    }
    if let Some(ref askpass) = askpass_file {
        add_cmd
            .env("SSH_ASKPASS", askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "susi:0");
    }
    add_cmd.arg(&key_file);
    let out = susi_paths::console::run_output_within(
        &mut add_cmd,
        None,
        std::time::Duration::from_secs(10),
    );
    let _ = super::vault_store::write_private_vault_bytes(&key_file, &[]);
    let _ = std::fs::remove_dir_all(&tmp_dir);

    let out = out.map_err(|e| format!("run ssh-add: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("ssh-add failed: {stderr}"));
    }
    Ok(true)
}

/// Prime GPG and (optionally) SSH signing agents from `SealedVault` (`signing` scope).
pub fn prime_signing_agents(
    config_dir: &Path,
    gpg_only: bool,
) -> Result<SigningPrimeReport, String> {
    let _ = auto_sync_if_enrolled(config_dir);
    let vault = SealedVault::open(config_dir)?.ok_or_else(|| {
        "no sealed vault found; run `susi keys vault enroll` and `susi keys vault sync` first"
            .to_string()
    })?;

    let mut notes = Vec::new();
    let real_gpg = resolve_real_gpg_binary();
    let gpgconf = resolve_gpgconf_binary(real_gpg.as_deref());
    let mut gpg_primed_keygrips = Vec::new();

    if let Some(gpg_pass) = vault.get("signing", "GPG_SIGNING_PASSPHRASE") {
        let hint = vault
            .get("signing", "GPG_SIGNING_FINGERPRINT")
            .or_else(|| vault.get("signing", "GPG_SIGNING_KEY_ID"))
            .map(ToOwned::to_owned)
            .or_else(|| git_global_get("user.signingkey"));
        let mut keygrips = Vec::new();
        if let Some(explicit_grip) = vault.get("signing", "GPG_SIGNING_KEYGRIP") {
            let clean = explicit_grip.trim().to_ascii_uppercase();
            if !clean.is_empty() {
                keygrips.push(clean);
            }
        }
        if let Some(ref gpg) = real_gpg {
            for discovered in discover_gpg_keygrips(gpg, hint.as_deref()) {
                if !keygrips.iter().any(|k| k == &discovered) {
                    keygrips.push(discovered);
                }
            }
        }
        gpg_primed_keygrips =
            preset_gpg_passphrase(gpg_pass, real_gpg.as_deref(), gpgconf.as_deref(), &keygrips)?;
    } else {
        notes.push("GPG_SIGNING_PASSPHRASE not present in sealed vault signing scope".to_string());
    }

    let mut ssh_primed = false;
    if !gpg_only {
        if let Some(priv_key) = vault.get("signing", "SSH_SIGNING_PRIVATE_KEY") {
            let ssh_pass = vault.get("signing", "SSH_SIGNING_PASSPHRASE");
            let fp = vault.get("signing", "SSH_SIGNING_FINGERPRINT");
            let pub_k = vault.get("signing", "SSH_SIGNING_PUBLIC_KEY");
            match prime_ssh_signing_identity(priv_key, ssh_pass, fp, pub_k) {
                Ok(ok) => ssh_primed = ok,
                Err(e) => notes.push(format!("SSH signing prime skipped/failed: {e}")),
            }
        }
    }

    Ok(SigningPrimeReport {
        gpg_primed_keygrips,
        ssh_primed,
        notes,
    })
}

fn gpg_args_need_signing_prime(args: &[String]) -> bool {
    let mut is_signing = false;
    let mut is_non_signing = false;
    for arg in args {
        let a = arg.as_str();
        if a == "--" {
            break;
        }
        if matches!(
            a,
            "--verify" | "--version" | "--list-keys" | "--list-secret-keys"
        ) {
            is_non_signing = true;
        }
        if matches!(
            a,
            "-s" | "--sign" | "-b" | "--detach-sign" | "--clearsign" | "-sb" | "-bs" | "-bsa"
        ) {
            is_signing = true;
        } else if a.starts_with('-') && !a.starts_with("--") {
            // Combined short options like -bsau or -absu
            let flags = &a[1..];
            if flags.contains('s') || flags.contains('b') {
                is_signing = true;
            }
        }
    }
    is_signing && !is_non_signing
}

/// Drop-in zero-daemon Git `gpg.program` executor across Windows, Linux, and macOS.
pub fn run_git_gpg_program(config_dir: &Path, args: &[String]) -> i32 {
    let Some(real_gpg) = resolve_real_gpg_binary() else {
        eprintln!("susi-gpg: real `gpg` executable not found in PATH or standard directories");
        return 127;
    };
    if gpg_args_need_signing_prime(args) {
        let _ = prime_signing_agents(config_dir, true);
    }
    let mut cmd = Command::new(&real_gpg);
    cmd.args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("susi-gpg: failed to execute `{real_gpg}`: {e}");
            1
        }
    }
}

/// Fast-path `argv[0]` multicall dispatch when invoked as `susi-gpg` / `susi-git-gpg`.
#[must_use]
pub fn dispatch_gpg_program_multicall() -> Option<std::process::ExitCode> {
    let mut args = std::env::args();
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
    let rest: Vec<String> = args.collect();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    let code = run_git_gpg_program(&cfg_dir, &rest);
    let u8_code = u8::try_from(code).unwrap_or(1);
    Some(std::process::ExitCode::from(u8_code))
}

/// Install the `susi-gpg` wrapper into `~/.susi/bin` and repair cross-OS Git signing config.
pub fn setup_git_signing(
    config_dir: &Path,
    enable_gpgsign: bool,
) -> Result<GitSetupReport, String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("resolve current susi binary: {e}"))?;
    let bin_dir = config_dir.join("bin");
    create_private_dir(&bin_dir).map_err(|e| format!("create {}: {e}", bin_dir.display()))?;

    #[cfg(windows)]
    let wrapper_name = "susi-gpg.exe";
    #[cfg(not(windows))]
    let wrapper_name = "susi-gpg";
    let wrapper_path = bin_dir.join(wrapper_name);

    #[cfg(unix)]
    {
        if wrapper_path.exists() || std::fs::symlink_metadata(&wrapper_path).is_ok() {
            let _ = std::fs::remove_file(&wrapper_path);
        }
        std::os::unix::fs::symlink(&current_exe, &wrapper_path)
            .map_err(|e| format!("symlink `{}`: {e}", wrapper_path.display()))?;
    }
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(&wrapper_path);
        if std::fs::hard_link(&current_exe, &wrapper_path).is_err() {
            std::fs::copy(&current_exe, &wrapper_path)
                .map_err(|e| format!("install wrapper `{}`: {e}", wrapper_path.display()))?;
        }
    }

    let normalized_wrapper = wrapper_path.to_string_lossy().replace('\\', "/");
    let set_git = |key: &str, val: &str| -> Result<(), String> {
        let mut cmd = Command::new("git");
        cmd.args(["config", "--global", key, val]);
        let out = susi_paths::console::run_output_within(
            &mut cmd,
            None,
            std::time::Duration::from_secs(5),
        )
        .map_err(|e| format!("run git config: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("git config --global {key} failed: {err}"));
        }
        Ok(())
    };
    set_git("gpg.program", &normalized_wrapper)?;

    let mut ssh_command_action = "unset".to_string();
    if let Some(existing_ssh) = git_global_get("core.sshCommand") {
        if !cfg!(windows) && is_windows_only_git_path(&existing_ssh) {
            let mut cmd = Command::new("git");
            cmd.args(["config", "--global", "--unset", "core.sshCommand"]);
            let _ = susi_paths::console::run_output_within(
                &mut cmd,
                None,
                std::time::Duration::from_secs(5),
            );
            ssh_command_action = "removed-windows-path".to_string();
        } else {
            ssh_command_action = "kept".to_string();
        }
    }

    let mut signing_key_set = git_global_get("user.signingkey");
    if signing_key_set.is_none() {
        if let Ok(Some(vault)) = SealedVault::open(config_dir) {
            if let Some(k) = vault
                .get("signing", "GPG_SIGNING_KEY_ID")
                .or_else(|| vault.get("signing", "GPG_SIGNING_FINGERPRINT"))
            {
                let trimmed = k.trim().to_string();
                if !trimmed.is_empty() {
                    set_git("user.signingkey", &trimmed)?;
                    signing_key_set = Some(trimmed);
                }
            }
        }
    }

    let mut commit_gpgsign_enabled =
        git_global_get("commit.gpgsign").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if enable_gpgsign && !commit_gpgsign_enabled {
        set_git("commit.gpgsign", "true")?;
        commit_gpgsign_enabled = true;
    }

    Ok(GitSetupReport {
        wrapper_path,
        gpg_program_set: normalized_wrapper,
        ssh_command_action,
        signing_key_set,
        commit_gpgsign_enabled,
    })
}

/// Inspect cross-OS KeePassXC, keystore, D-Bus, Qt headless, and Git signing readiness.
#[must_use]
pub fn vault_doctor(config_dir: &Path) -> VaultDoctorReport {
    let spec = resolve_keepassxc_cli_spec(config_dir);
    let keepassxc_cli = resolve_keepassxc_cli(config_dir);
    let _ = &spec;

    let mut ver_cmd = Command::new(&spec.program);
    ver_cmd.args(&spec.prefix_args).arg("--version");
    let (keepassxc_available, keepassxc_version) = match susi_paths::console::run_output_within(
        &mut ver_cmd,
        None,
        std::time::Duration::from_secs(5),
    ) {
        Ok(o) if o.status.success() => (
            true,
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        ),
        _ => (false, None),
    };

    let kdbx_path = discover_kdbx_path(config_dir).ok().flatten();
    let kdbx_valid_magic = kdbx_path.as_deref().is_some_and(has_valid_kdbx_magic);
    let kdbx_key_file = discover_kdbx_key_file_for(config_dir, kdbx_path.as_deref());

    let mut legacy_dpapi_present = config_dir
        .join("vault")
        .join("automation-key.dpapi")
        .is_file();
    if let Some(ref db) = kdbx_path {
        if let Some(p) = db.parent() {
            legacy_dpapi_present |= p.join("automation-key.dpapi").is_file();
        }
    }
    if let Some(h) = home_dir() {
        legacy_dpapi_present |= h
            .join("KeePassXC-AppLauncher")
            .join("store")
            .join("automation-key.dpapi")
            .is_file();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    let dbus_bus = super::vault_store::resolve_dbus_session_bus();
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let dbus_bus: Option<String> = None;

    let headless_qt_needed = is_headless_qt_env();
    let real_gpg_path = resolve_real_gpg_binary();
    let gpgconf = resolve_gpgconf_binary(real_gpg_path.as_deref());
    let gpg_preset_passphrase_path = resolve_gpg_preset_passphrase_binary(real_gpg_path.as_deref());
    let gpg_agent_allow_preset = resolve_gnupg_homedir(gpgconf.as_deref())
        .and_then(|d| std::fs::read_to_string(d.join("gpg-agent.conf")).ok())
        .is_some_and(|raw| {
            raw.lines()
                .any(|l| l.split('#').next().unwrap_or("").trim() == "allow-preset-passphrase")
        });

    let git_gpg_program = git_global_get("gpg.program");
    let git_ssh_command = git_global_get("core.sshCommand");
    let git_signing_key = git_global_get("user.signingkey");
    let gpg_keygrips = real_gpg_path
        .as_deref()
        .map(|g| discover_gpg_keygrips(g, git_signing_key.as_deref()))
        .unwrap_or_default();
    let ssh_auth_sock = resolve_ssh_auth_sock();
    let ssh_add_path = resolve_ssh_add_binary();

    let opened_vault = SealedVault::open(config_dir).ok().flatten();
    let vault_has_gpg_passphrase = opened_vault
        .as_ref()
        .is_some_and(|v| v.get("signing", "GPG_SIGNING_PASSPHRASE").is_some());
    let vault_has_ssh_identity = opened_vault
        .as_ref()
        .is_some_and(|v| v.get("signing", "SSH_SIGNING_PRIVATE_KEY").is_some());

    let mut issues = Vec::new();
    let mut recommendations = Vec::new();

    if !keepassxc_available {
        issues.push(format!("`{keepassxc_cli}` could not be executed"));
        recommendations.push(
            "Install KeePassXC (`pacman -S keepassxc` on CachyOS, `apt install keepassxc` or Flatpak `org.keepassxc.KeePassXC` on Linux Mint)"
                .to_string(),
        );
    }
    if let Some(ref db) = kdbx_path {
        if !kdbx_valid_magic {
            issues.push(format!(
                "KeePassXC database `{}` is corrupt or truncated (missing KDBX magic header)",
                db.display()
            ));
            recommendations.push(
                "Restore a valid `.kdbx` from Google Drive / `rclone` or `.bak.*` backup"
                    .to_string(),
            );
        }
        if !cfg!(windows) && legacy_dpapi_present && kdbx_key_file.is_none() {
            issues.push(
                "Detected Windows DPAPI `automation-key.dpapi` with no portable `.keyx` keyfile; Windows DPAPI secrets cannot unlock on Linux"
                    .to_string(),
            );
            recommendations.push(
                "Run `susi keys vault rekey` on Windows to add a portable passphrase and `.keyx` sidecar, then sync both to Google Drive/Linux and run `susi keys vault enroll --enroll-kdbx-passphrase`"
                    .to_string(),
            );
        } else if cfg!(windows) && legacy_dpapi_present {
            recommendations.push(
                "Windows-only `automation-key.dpapi` is present; run `susi keys vault rekey` to convert `Automation.kdbx` to a cross-OS passphrase + `.keyx` sidecar for CachyOS/Mint"
                    .to_string(),
            );
        }
    } else {
        issues.push("No Automation.kdbx or AI-Keys.kdbx database found".to_string());
    }

    if let Some(ref prog) = git_gpg_program {
        if !cfg!(windows) && is_windows_only_git_path(prog) {
            issues.push(format!(
                "Global git `gpg.program` points to Windows path `{prog}`"
            ));
            recommendations
                .push("Run `susi keys vault setup-git` to repair `.gitconfig`".to_string());
        }
    }
    if let Some(ref ssh_cmd) = git_ssh_command {
        if !cfg!(windows) && is_windows_only_git_path(ssh_cmd) {
            issues.push(format!(
                "Global git `core.sshCommand` points to Windows path `{ssh_cmd}`"
            ));
            recommendations.push(
                "Run `susi keys vault setup-git` to remove Windows `core.sshCommand`".to_string(),
            );
        }
    }
    if !gpg_agent_allow_preset && real_gpg_path.is_some() {
        recommendations.push(
            "Run `susi keys vault prime-signing` or `susi keys vault setup-git` to enable `allow-preset-passphrase` in gpg-agent.conf"
                .to_string(),
        );
    }
    if cfg!(target_os = "linux") && dbus_bus.is_none() {
        recommendations.push(
            "D-Bus session bus socket not detected; Linux hosts can use `systemd-creds` (`systemctl --user`) or `keyfile` backend"
                .to_string(),
        );
    }

    VaultDoctorReport {
        os: std::env::consts::OS.to_string(),
        config_dir: config_dir.to_path_buf(),
        keepassxc_cli,
        keepassxc_available,
        keepassxc_version,
        kdbx_path,
        kdbx_valid_magic,
        kdbx_key_file,
        legacy_dpapi_present,
        dbus_bus,
        headless_qt_needed,
        real_gpg_path,
        gpg_preset_passphrase_path,
        gpg_agent_allow_preset,
        gpg_keygrips,
        git_gpg_program,
        git_ssh_command,
        git_signing_key,
        ssh_auth_sock,
        ssh_add_path,
        vault_has_gpg_passphrase,
        vault_has_ssh_identity,
        issues,
        recommendations,
    }
}

/// Options controlling [`rekey_kdbx_portable`].
#[derive(Debug, Clone, Default)]
pub struct RekeyOptions<'a> {
    pub db_override: Option<&'a Path>,
    pub keyfile_out: Option<&'a Path>,
    pub generate_keyfile: bool,
    pub new_passphrase: Option<&'a str>,
    pub allow_interactive: bool,
}

/// Convert an existing `.kdbx` (including Windows DPAPI-bound `Automation.kdbx`) to use
/// cross-OS portable credentials (`.keyx` keyfile and/or portable passphrase).
pub fn rekey_kdbx_portable(
    config_dir: &Path,
    opts: RekeyOptions<'_>,
) -> Result<RekeyReport, String> {
    let db_path = if let Some(explicit) = opts.db_override {
        validate_non_personal(explicit)?;
        explicit.to_path_buf()
    } else {
        discover_kdbx_path(config_dir)?.ok_or_else(|| {
            "no Automation/AI-Keys KeePassXC database found; pass --db <path>".to_string()
        })?
    };
    if !db_path.is_file() || !has_valid_kdbx_magic(&db_path) {
        return Err(format!(
            "cannot rekey `{}`: file missing or lacks valid KDBX header",
            db_path.display()
        ));
    }
    let clean_new_pass = opts
        .new_passphrase
        .map(|s| s.trim_end_matches(&['\r', '\n'][..]).to_string())
        .filter(|s| !s.is_empty());

    let target_kf = if let Some(explicit_kf) = opts.keyfile_out {
        validate_non_personal(explicit_kf)?;
        Some(explicit_kf.to_path_buf())
    } else if opts.generate_keyfile || clean_new_pass.is_none() {
        let parent = db_path.parent().unwrap_or(config_dir);
        let stem = db_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Automation");
        Some(parent.join(format!("{stem}.keyx")))
    } else {
        None
    };

    let spec = resolve_keepassxc_cli_spec(config_dir);
    let auth = resolve_kdbx_auth_for(config_dir, Some(&db_path), opts.allow_interactive)?;

    if clean_new_pass.is_none() && target_kf.is_none() {
        return Err(
            "rekey requires a new passphrase, a target `.keyx` keyfile, or both".to_string(),
        );
    }

    // Create timestamped backup before editing credentials.
    let backup_path = PathBuf::from(format!("{}.bak.{}", db_path.display(), now_unix()));
    atomic_copy_file(&db_path, &backup_path)?;

    let created_new_kf = if let Some(ref kf) = target_kf {
        if !kf.is_file() {
            let mut key_bytes = [0u8; 32];
            getrandom::fill(&mut key_bytes)
                .map_err(|e| format!("generate portable .keyx material: {e}"))?;
            if let Some(parent) = kf.parent() {
                create_private_dir(parent)
                    .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
            }
            let write_res = super::vault_store::write_private_vault_bytes(kf, &key_bytes)
                .map_err(|e| format!("write {}: {e}", kf.display()));
            super::vault_store::wipe_bytes(&mut key_bytes);
            write_res?;
            true
        } else {
            false
        }
    } else {
        false
    };

    let mut cmd = Command::new(&spec.program);
    let headless_qt = is_headless_qt_env();
    if headless_qt {
        cmd.env("QT_QPA_PLATFORM", "offscreen")
            .env("QT_LOGGING_RULES", "qt.qpa.*=false");
    }
    if spec.is_flatpak {
        let insert_at = if spec.prefix_args.first().is_some_and(|a| a == "run") {
            1usize
        } else {
            0usize
        };
        cmd.args(&spec.prefix_args[..insert_at]);
        if headless_qt {
            cmd.arg("--env=QT_QPA_PLATFORM=offscreen")
                .arg("--env=QT_LOGGING_RULES=qt.qpa.*=false");
        }
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            cmd.arg(format!("--filesystem={}", parent.display()));
        }
        if let Some(kf_parent) = target_kf
            .as_deref()
            .or(auth.key_file.as_deref())
            .and_then(|p| p.parent())
            .filter(|p| !p.as_os_str().is_empty())
        {
            cmd.arg(format!("--filesystem={}:ro", kf_parent.display()));
        }
        cmd.args(&spec.prefix_args[insert_at..]);
    } else {
        cmd.args(&spec.prefix_args);
    }
    cmd.arg("db-edit");
    if auth.passphrase.is_none() && auth.key_file.is_some() {
        cmd.arg("--no-password");
    }
    if let Some(ref old_kf) = auth.key_file {
        cmd.arg("-k").arg(old_kf);
    }
    if clean_new_pass.is_some() {
        cmd.arg("-p");
    } else {
        cmd.arg("--unset-password");
    }
    if let Some(ref new_kf) = target_kf {
        cmd.arg("--set-key-file").arg(new_kf);
    } else if auth.key_file.is_some() {
        cmd.arg("--unset-key-file");
    }
    cmd.arg("--").arg(&db_path);

    let mut stdin_buf = String::new();
    if let Some(ref old_p) = auth.passphrase {
        stdin_buf.push_str(old_p);
        stdin_buf.push('\n');
    }
    if let Some(ref new_p) = clean_new_pass {
        stdin_buf.push_str(new_p);
        stdin_buf.push('\n');
        stdin_buf.push_str(new_p);
        stdin_buf.push('\n');
    }
    let out = susi_paths::console::run_output_within(
        &mut cmd,
        Some(stdin_buf.as_bytes()),
        KEEPASSXC_TIMEOUT,
    );
    super::vault_store::wipe_string(&mut stdin_buf);
    let out = out.map_err(|e| {
        if created_new_kf {
            if let Some(ref kf) = target_kf {
                let _ = std::fs::remove_file(kf);
            }
        }
        format!("failed to run `{}` db-edit: {e}", spec.display)
    })?;
    if !out.status.success() {
        if created_new_kf {
            if let Some(ref kf) = target_kf {
                let _ = std::fs::remove_file(kf);
            }
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let detail = if !err.is_empty() {
            err
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit status {:?}", out.status.code())
        };
        return Err(format!(
            "`{}` db-edit failed (original preserved at {}): {detail}",
            spec.display,
            backup_path.display()
        ));
    }

    // Verify unlock succeeds with the new credentials before enrolling.
    let verify_auth = KdbxAuthMaterial {
        passphrase: clean_new_pass.clone(),
        key_file: target_kf.clone(),
        interactive_tty: false,
    };
    let verify_cli = spec.display.clone();
    run_keepassxc(&verify_cli, &["ls", "-R"], &db_path, &[], &verify_auth)?;

    let kdbx_unlock_backend = if let Some(ref new_p) = clean_new_pass {
        Some(super::vault_store::enroll_kdbx_passphrase(config_dir, new_p, None)?.to_string())
    } else {
        None
    };

    let mut cfg = VaultConfig::load(config_dir);
    cfg.vault_db_path = Some(db_path.clone());
    cfg.key_file_path = target_kf.clone();
    cfg.save(config_dir)?;

    Ok(RekeyReport {
        db_path,
        backup_path,
        key_file: target_kf,
        has_passphrase: clean_new_pass.is_some(),
        kdbx_unlock_backend,
    })
}

#[cfg(test)]
// Unit-test module only; production code in this module uses explicit Result handling.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct EnvRestoreGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl EnvRestoreGuard {
        fn clear(keys: &[&'static str]) -> Self {
            let saved = keys
                .iter()
                .map(|&k| {
                    let prev = std::env::var_os(k);
                    std::env::remove_var(k);
                    (k, prev)
                })
                .collect();
            Self(saved)
        }
    }
    impl Drop for EnvRestoreGuard {
        fn drop(&mut self) {
            for (k, v) in self.0.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

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

        let _env = EnvRestoreGuard::clear(&[
            super::super::vault_store::VAULT_KEY_ENV,
            super::super::vault_store::KDBX_PASSPHRASE_ENV,
            VAULT_KEY_FILE_ENV,
            SUSI_VAULT_KEY_FILE_ENV,
        ]);
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

    #[test]
    fn cross_os_keepassxc_gdrive_and_profile_expansion_work() {
        let _guard = crate::env_test_lock();
        let dir = scratch_dir("cross_os_paths");
        let fake_drive = dir
            .join("Library")
            .join("CloudStorage")
            .join("GoogleDrive-user@example.com")
            .join("My Drive")
            .join("Vault");
        std::fs::create_dir_all(&fake_drive).unwrap();
        let fake_kdbx = fake_drive.join("AI-Keys.kdbx");
        std::fs::write(&fake_kdbx, b"test").unwrap();

        let mut mac_paths = Vec::new();
        append_macos_cloud_drive_candidates(&dir, &mut mac_paths);
        assert!(mac_paths.contains(&fake_kdbx));

        let win_clis = windows_keepassxc_candidates(
            Some(r"C:\Program Files"),
            None,
            Some(r"C:\Program Files (Arm)"),
            Some(r"C:\Users\u\AppData\Local"),
            Some(r"C:\Users\u"),
        );
        assert!(win_clis.iter().any(|p| {
            p.to_string_lossy()
                .replace('\\', "/")
                .contains("Program Files (Arm)/KeePassXC/keepassxc-cli.exe")
        }));
        assert!(win_clis.iter().any(|p| {
            p.to_string_lossy()
                .replace('\\', "/")
                .contains("WinGet/Links/keepassxc-cli.exe")
        }));

        let mac_clis = macos_keepassxc_candidates(Some(&dir));
        assert!(mac_clis
            .iter()
            .any(|p| p == Path::new("/Applications/KeePassXC.app/Contents/MacOS/keepassxc-cli")));
        assert!(mac_clis
            .iter()
            .any(|p| p == Path::new("/opt/homebrew/bin/keepassxc-cli")));

        assert!(map_kdbx_entry("AI/qwen")
            .iter()
            .any(|(s, k)| *s == "cloud" && k == "DASHSCOPE_API_KEY"));
        assert!(map_kdbx_entry("AI/kimi")
            .iter()
            .any(|(s, k)| *s == "cloud" && k == "KIMI_API_KEY"));

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
    fn linux_keepassxc_flatpak_snap_and_headless_qt_work() {
        let dir = scratch_dir("linux_cli");
        // 1. Snap/native binary wins over Flatpak when present.
        let snap_bin = PathBuf::from("/snap/bin/keepassxc.cli");
        let spec_snap = detect_linux_keepassxc_spec_with(
            None,
            Some(&dir),
            |p| p == snap_bin,
            |_| false,
            |_| true,
        );
        assert_eq!(spec_snap.program, "/snap/bin/keepassxc.cli");
        assert!(!spec_snap.is_flatpak);

        // 2. Linux Mint Flatpak detection builds `flatpak run --command=keepassxc-cli org.keepassxc.KeePassXC`.
        let fp_app = PathBuf::from("/var/lib/flatpak/app/org.keepassxc.KeePassXC");
        let fp_bin = PathBuf::from("/usr/bin/flatpak");
        let spec_fp = detect_linux_keepassxc_spec_with(
            None,
            Some(&dir),
            |p| p == fp_bin,
            |p| p == fp_app,
            |_| false,
        );
        assert!(spec_fp.is_flatpak);
        assert_eq!(spec_fp.program, "/usr/bin/flatpak");
        assert_eq!(
            spec_fp.prefix_args,
            vec!["run", "--command=keepassxc-cli", KEEPASSXC_FLATPAK_APP_ID]
        );
        assert_eq!(spec_fp.display, "flatpak:org.keepassxc.KeePassXC");

        // 3. Headless Qt helper enables offscreen only when X11 and Wayland displays are absent.
        assert!(should_set_qt_offscreen(None, None, None));
        assert!(should_set_qt_offscreen(Some(""), Some(" "), None));
        assert!(!should_set_qt_offscreen(Some(":0"), None, None));
        assert!(!should_set_qt_offscreen(None, Some("wayland-0"), None));
        assert!(!should_set_qt_offscreen(None, None, Some("xcb")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_keyfile_and_corrupt_magic_fallback_work() {
        let _guard = crate::env_test_lock();
        let dir = scratch_dir("sidecar_magic");
        let cfg_dir = dir.join("cfg");
        let home_dir_p = dir.join("home");
        let bad_db = home_dir_p.join("Vault").join("Automation.kdbx");
        let good_db = home_dir_p.join("Susi-Fleet-Vault").join("AI-Keys.kdbx");
        std::fs::create_dir_all(bad_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(good_db.parent().unwrap()).unwrap();
        std::fs::write(&bad_db, vec![b'!'; 272]).unwrap();
        let mut valid_bytes = KDBX_MAGIC.to_vec();
        valid_bytes.extend_from_slice(&[0u8; 32]);
        std::fs::write(&good_db, &valid_bytes).unwrap();
        let sidecar = home_dir_p.join("Susi-Fleet-Vault").join("AI-Keys.keyx");
        std::fs::write(&sidecar, b"sidecar-key-bytes").unwrap();

        let prev_home = std::env::var_os("HOME");
        let prev_up = std::env::var_os("USERPROFILE");
        let prev_db = std::env::var_os(VAULT_DB_ENV);
        let prev_cdb = std::env::var_os(COMMERCIAL_VAULT_ENV);
        let prev_kf = std::env::var_os(SUSI_VAULT_KEY_FILE_ENV);
        let prev_kf2 = std::env::var_os(VAULT_KEY_FILE_ENV);
        std::env::set_var("HOME", &home_dir_p);
        std::env::set_var("USERPROFILE", &home_dir_p);
        std::env::remove_var(VAULT_DB_ENV);
        std::env::remove_var(COMMERCIAL_VAULT_ENV);
        std::env::remove_var(SUSI_VAULT_KEY_FILE_ENV);
        std::env::remove_var(VAULT_KEY_FILE_ENV);

        let discovered = discover_kdbx_path(&cfg_dir).unwrap().unwrap();
        assert_eq!(discovered, good_db);
        let kf = discover_kdbx_key_file_for(&cfg_dir, Some(&discovered)).unwrap();
        assert_eq!(kf, sidecar);

        // Personal sidecar paths must still be refused.
        let personal_db = dir.join("Personal.kdbx");
        assert!(discover_kdbx_key_file_for(&cfg_dir, Some(&personal_db)).is_none());

        for (k, v) in [
            ("HOME", prev_home),
            ("USERPROFILE", prev_up),
            (VAULT_DB_ENV, prev_db),
            (COMMERCIAL_VAULT_ENV, prev_cdb),
            (SUSI_VAULT_KEY_FILE_ENV, prev_kf),
            (VAULT_KEY_FILE_ENV, prev_kf2),
        ] {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xml_export_parser_imports_custom_attributes_and_signing_entries() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<KeePassFile><Root><Group><Name>Passwords</Name>
  <Group><Name>App credentials</Name>
    <Entry>
      <String><Key>Title</Key><Value>Forward to apps</Value></String>
      <String><Key>Password</Key><Value ProtectInMemory="True"></Value></String>
      <String><Key>Notes</Key><Value>DEEPSEEK_API_KEY=ds-notes-1</Value></String>
      <String><Key>OPENAI_API_KEY</Key><Value ProtectInMemory="True">sk-custom-amp-&amp;-1</Value></String>
      <String><Key>GH_TOKEN</Key><Value ProtectInMemory="True">gho_custom_tok</Value></String>
      <History><Entry>
        <String><Key>Title</Key><Value>Forward to apps</Value></String>
        <String><Key>OPENAI_API_KEY</Key><Value>sk-stale-history</Value></String>
      </Entry></History>
    </Entry>
    <Entry>
      <String><Key>Title</Key><Value>GPG signing</Value></String>
      <String><Key>UserName</Key><Value>102DE0E0B3923347D2389C2150AC4B1B0B4C1088</Value></String>
      <String><Key>Password</Key><Value ProtectInMemory="True">gpg-secret-pass</Value></String>
      <String><Key>Notes</Key><Value>KEY_ID=A18641EA74E38BA1</Value></String>
    </Entry>
    <Entry>
      <String><Key>Title</Key><Value>SSH signing identity</Value></String>
      <String><Key>Password</Key><Value ProtectInMemory="True">ssh-pass</Value></String>
      <String><Key>Notes</Key><Value>ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAItest</Value></String>
      <Binary><Key>id_ed25519</Key><Value Ref="0"/></Binary>
    </Entry>
  </Group>
  <Group><Name>Recycle Bin</Name>
    <Entry>
      <String><Key>Title</Key><Value>openai</Value></String>
      <String><Key>Password</Key><Value>sk-deleted</Value></String>
    </Entry>
  </Group>
</Group></Root></KeePassFile>"#;

        let records = parse_kdbx_xml_entries(xml);
        assert_eq!(records.len(), 3);
        let mut vault = SealedVault::default();
        let mut cloud = BTreeSet::new();
        let mut comm = BTreeSet::new();
        let mut signing = BTreeSet::new();
        let mut targets = VaultImportTargets {
            vault: &mut vault,
            cloud: &mut cloud,
            commercial: &mut comm,
            signing: &mut signing,
        };
        for rec in records {
            if rec.entry_path.ends_with("SSH signing identity") {
                assert_eq!(rec.attachment_names, vec!["id_ed25519".to_string()]);
            }
            import_kdbx_entry_record(&mut targets, rec);
        }
        assert_eq!(
            vault.get("cloud", "OPENAI_API_KEY"),
            Some("sk-custom-amp-&-1")
        );
        assert_eq!(vault.get("cloud", "DEEPSEEK_API_KEY"), Some("ds-notes-1"));
        assert_eq!(vault.get("cloud", "GH_TOKEN"), Some("gho_custom_tok"));
        assert_eq!(vault.get("cloud", "GITHUB_TOKEN"), Some("gho_custom_tok"));
        assert_eq!(
            vault.get("signing", "GPG_SIGNING_PASSPHRASE"),
            Some("gpg-secret-pass")
        );
        assert_eq!(
            vault.get("signing", "GPG_SIGNING_KEYGRIP"),
            Some("102DE0E0B3923347D2389C2150AC4B1B0B4C1088")
        );
        assert_eq!(
            vault.get("signing", "GPG_SIGNING_KEY_ID"),
            Some("A18641EA74E38BA1")
        );
        assert_eq!(
            vault.get("signing", "SSH_SIGNING_PUBLIC_KEY"),
            Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAItest")
        );
    }

    #[test]
    fn gpg_keygrip_parser_git_remediation_and_wrapper_guard_work() {
        let colons = "\
sec:u:4096:1:A18641EA74E38BA1:1780787855:::u:::scESC:::+:::23::0:
fpr:::::::::A2D6C27F2D77F13C37908A74A18641EA74E38BA1:
grp:::::::::102DE0E0B3923347D2389C2150AC4B1B0B4C1088:
uid:u::::1780787855::HASH::User <u@example.com>::::::::::0:
ssb:u:4096:1:955F4451E273CC8C:1780787855::::::e:::+:::23:
fpr:::::::::D19721D8C1621F661A13557F955F4451E273CC8C:
grp:::::::::14C54350291B1C780F89044E4C57A8F2C7F41DDF:
";
        let grips = parse_gpg_colons_keygrips(colons, Some("A18641EA74E38BA1"));
        assert!(grips.contains(&"102DE0E0B3923347D2389C2150AC4B1B0B4C1088".to_string()));
        assert!(is_windows_only_git_path(
            "C:/WINDOWS/System32/OpenSSH/ssh.exe"
        ));
        assert!(is_windows_only_git_path(
            "C:/Users/user/KeePassXC-AppLauncher/git-gpg-agent.exe"
        ));
        assert!(!is_windows_only_git_path("/usr/bin/ssh"));
        assert!(is_susi_wrapper_binary(Path::new(
            "/home/user/.susi/bin/susi-gpg"
        )));
        assert!(is_susi_wrapper_binary(Path::new(r"C:\susi-gpg.exe")));
        assert!(!is_susi_wrapper_binary(Path::new("/usr/bin/gpg")));
        assert!(gpg_args_need_signing_prime(&[
            "-bsau".to_string(),
            "A18641EA74E38BA1".to_string()
        ]));
        assert!(!gpg_args_need_signing_prime(&["--version".to_string()]));
        assert!(!gpg_args_need_signing_prime(&[
            "--verify".to_string(),
            "sig.asc".to_string()
        ]));

        assert_eq!(
            resolve_ssh_auth_sock_with(None, Some("/run/user/1000"), |p| {
                p == Path::new("/run/user/1000/gcr/ssh")
            }),
            Some("/run/user/1000/gcr/ssh".to_string())
        );
    }

    #[test]
    fn rekey_kdbx_portable_creates_keyx_and_rotates_real_database() {
        let cli_ok = Command::new("keepassxc-cli")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !cli_ok {
            return;
        }
        let _guard = crate::env_test_lock();
        let dir = scratch_dir("rekey_real");
        let cfg_dir = dir.join("cfg");
        let db = dir.join("Automation.kdbx");
        let _env = EnvRestoreGuard::clear(&[
            super::super::vault_store::VAULT_KEY_ENV,
            super::super::vault_store::KDBX_PASSPHRASE_ENV,
            SUSI_VAULT_KEY_FILE_ENV,
            VAULT_KEY_FILE_ENV,
        ]);

        enroll_master_key(&cfg_dir, Some("keyfile"), None).unwrap();

        let mut create = Command::new("keepassxc-cli")
            .args(["db-create", "-q", "-p"])
            .arg(&db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut stdin) = create.stdin.take() {
            let _ = stdin.write_all(b"old-rekey-pass\nold-rekey-pass\n");
        }
        assert!(create.wait_with_output().unwrap().status.success());

        std::env::set_var(
            super::super::vault_store::KDBX_PASSPHRASE_ENV,
            "old-rekey-pass",
        );
        let report = rekey_kdbx_portable(
            &cfg_dir,
            RekeyOptions {
                db_override: Some(&db),
                keyfile_out: None,
                generate_keyfile: true,
                new_passphrase: Some("new-portable-pass"),
                allow_interactive: false,
            },
        )
        .unwrap();
        assert!(report.backup_path.is_file());
        let keyx = report.key_file.unwrap();
        assert!(keyx.is_file());

        // Clear env overrides so new keyfile + host-enrolled passphrase must unlock DB.
        std::env::remove_var(super::super::vault_store::KDBX_PASSPHRASE_ENV);
        let entries = list_kdbx_entries(&cfg_dir, &db, false).unwrap();
        let _ = entries;

        let _ = std::fs::remove_dir_all(&dir);
    }
}
