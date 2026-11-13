//! `susi keys` — cloud API key lifecycle (set / list / prefer / remove).
//! Never prints secret material; `set` accepts a piped key on stdin or prompts.

use super::defs::{KeyCommands, VaultCommands};

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

pub(crate) fn run(action: Option<KeyCommands>) {
    match action {
        None | Some(KeyCommands::List) => print_keys_status(),
        Some(KeyCommands::Set { vendor, api_key }) => {
            let key = match api_key {
                Some(k) if !k.trim().is_empty() => k,
                _ => match prompt_api_key(&vendor) {
                    Ok(k) => k,
                    Err(e) => {
                        eprintln!("Failed to read API key: {}", e);
                        std::process::exit(1);
                    }
                },
            };
            match susi_gemi::engines::http_provider::register_api_key(&vendor, &key) {
                Ok(msg) => {
                    println!("{}", msg);
                    clear_quarantine_for(&vendor);
                }
                Err(e) => {
                    eprintln!("Key registration failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(KeyCommands::Prefer { vendor, clear }) => {
            match susi_daemon::provider_ops::keys_prefer(vendor.as_deref(), clear) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(KeyCommands::Check { vendor }) => run_check(vendor.as_deref()),
        Some(KeyCommands::Models { vendor }) => run_models(&vendor),
        Some(KeyCommands::Remove { vendor }) => {
            match susi_gemi::engines::http_provider::remove_api_key(&vendor) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Key removal failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(KeyCommands::Vault { action }) => run_vault(action),
    }
}

fn run_vault(action: VaultCommands) {
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    match action {
        VaultCommands::Status { json } => {
            let status = susi_config::vault_store::vault_status(&cfg_dir);
            if json {
                if let Ok(text) = serde_json::to_string_pretty(&status) {
                    println!("{text}");
                }
                return;
            }
            println!("Vault config dir: {}", status.config_dir.display());
            println!(
                "Sealed vault:     {} (entries: {})",
                if status.sealed_vault_exists {
                    if status.unlocked {
                        "sealed+unlocked"
                    } else {
                        "sealed (LOCKED)"
                    }
                } else {
                    "not initialized"
                },
                status.sealed_entry_count
            );
            println!(
                "Host unlock:      {}",
                status
                    .unlock_backend
                    .as_deref()
                    .unwrap_or("none (run `susi keys vault enroll` or set SUSI_VAULT_KEY)")
            );
            if let Some(fp) = &status.key_fingerprint {
                println!("Key fingerprint:  {fp}");
            }
            if let Some(err) = &status.unlock_error {
                println!("Unlock issue:     {err}");
            }
            println!(
                "KeePassXC DB:     {}",
                status
                    .vault_db_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none configured/found".to_string())
            );
            println!(
                "KDBX auto-unlock: {}",
                if status.kdbx_unlock_enrolled {
                    status.kdbx_unlock_backend.as_deref().unwrap_or("enrolled")
                } else {
                    "not enrolled"
                }
            );
            if let Some(gd) = &status.gdrive_sync_path {
                println!("GDrive mirror:    {}", gd.display());
            }
            if let Some(rc) = &status.rclone_remote {
                println!("rclone remote:    {rc}");
            }
            if status.plaintext_cloud_env_keys > 0 {
                println!(
                    "WARNING:          {} plaintext key(s) remain in cloud.env — run `susi keys vault migrate-cloud-env`",
                    status.plaintext_cloud_env_keys
                );
            }
        }
        VaultCommands::Enroll {
            backend,
            db,
            key_file,
            gdrive_path,
            rclone_remote,
            enroll_kdbx_passphrase,
        } => {
            if let Some(ref db_path) = db {
                if susi_config::keepass_bridge::is_personal_vault_path(db_path) {
                    eprintln!(
                        "Refusing `{}`: Personal KeePassXC vaults are off limits.",
                        db_path.display()
                    );
                    std::process::exit(1);
                }
            }
            if let Some(ref gd_path) = gdrive_path {
                if susi_config::keepass_bridge::is_personal_vault_path(gd_path) {
                    eprintln!(
                        "Refusing `{}`: Personal KeePassXC vaults are off limits.",
                        gd_path.display()
                    );
                    std::process::exit(1);
                }
            }
            let enrolled = match susi_config::vault_store::enroll_master_key(
                &cfg_dir,
                backend.as_deref(),
                None,
            ) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Vault enrollment failed: {e}");
                    std::process::exit(1);
                }
            };
            let mut cfg = susi_config::vault_store::VaultConfig::load(&cfg_dir);
            if db.is_some() {
                cfg.vault_db_path = db;
            }
            if key_file.is_some() {
                cfg.key_file_path = key_file;
            }
            if gdrive_path.is_some() {
                cfg.gdrive_sync_path = gdrive_path;
            }
            if let Some(remote) = rclone_remote {
                let clean = remote.trim().to_string();
                cfg.rclone_remote = (!clean.is_empty()).then_some(clean);
            }
            if let Err(e) = cfg.save(&cfg_dir) {
                eprintln!("Failed to save vault.json: {e}");
                std::process::exit(1);
            }
            println!("Enrolled sealed vault host unlock backend: {enrolled}");

            if enroll_kdbx_passphrase {
                let mut pass = match read_secret_prompt(
                    "Enter KeePassXC master passphrase (piped stdin supported): ",
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("Failed to read KeePassXC passphrase: {e}");
                        std::process::exit(1);
                    }
                };
                let kdbx_backend = susi_config::vault_store::enroll_kdbx_passphrase(
                    &cfg_dir,
                    &pass,
                    backend.as_deref(),
                );
                susi_config::vault_store::wipe_string(&mut pass);
                match kdbx_backend {
                    Ok(kb) => println!("Enrolled unattended KeePassXC unlock backend: {kb}"),
                    Err(e) => {
                        eprintln!("KeePassXC passphrase enrollment failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
        VaultCommands::Sync {
            db,
            direction,
            dry_run,
            json,
        } => {
            let dir = match susi_config::keepass_bridge::GDriveSyncDirection::parse(&direction) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            match susi_config::keepass_bridge::sync_kdbx_to_sealed_vault(
                &cfg_dir,
                db.as_deref(),
                dir,
                io::stdin().is_terminal() && io::stderr().is_terminal(),
                dry_run,
            ) {
                Ok(report) => {
                    if json {
                        if let Ok(text) = serde_json::to_string_pretty(&report) {
                            println!("{text}");
                        }
                    } else {
                        println!(
                            "Synced `{}` -> sealed vault ({} cloud, {} commercial, {} total sealed entries{})",
                            report.db_path.display(),
                            report.imported_cloud_keys.len(),
                            report.imported_commercial_keys.len(),
                            report.total_sealed_entries,
                            if report.dry_run { ", dry-run" } else { "" }
                        );
                        if let Some(gd) = &report.gdrive_report {
                            println!("Google Drive sync: {} ({})", gd.action, gd.remote_desc);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Vault sync failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        VaultCommands::MigrateCloudEnv { keep_plaintext } => {
            match susi_config::vault_store::migrate_plaintext_cloud_env(&cfg_dir, keep_plaintext) {
                Ok(summary) => {
                    println!(
                        "Migrated {} key(s) into sealed vault (plaintext cleared: {})",
                        summary.migrated_keys.len(),
                        summary.cloud_env_cleared
                    );
                }
                Err(e) => {
                    eprintln!("Migration failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        VaultCommands::InitCloudKey { key_out } => {
            let out_path: PathBuf = key_out.unwrap_or_else(|| cfg_dir.join("cloud-bootstrap.key"));
            let hex = match susi_config::vault_store::resolve_unlock_secret(
                &cfg_dir,
                susi_config::vault_store::PURPOSE_VAULT,
            ) {
                Ok(Some((raw, _))) => {
                    let key = susi_config::vault_store::normalize_master_key(&raw);
                    susi_paths::hex::encode(key)
                }
                Ok(None) => {
                    if let Err(e) =
                        susi_config::vault_store::enroll_master_key(&cfg_dir, None, None)
                    {
                        eprintln!("Failed to initialize vault key: {e}");
                        std::process::exit(1);
                    }
                    match susi_config::vault_store::resolve_master_key(&cfg_dir) {
                        Ok(Some(k)) => susi_paths::hex::encode(k),
                        _ => {
                            eprintln!("Unable to read initialized vault key");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to resolve vault key: {e}");
                    std::process::exit(1);
                }
            };
            if let Some(parent) = out_path.parent() {
                if let Err(e) = susi_config::create_private_dir(parent) {
                    eprintln!("Failed to create directory: {e}");
                    std::process::exit(1);
                }
            }
            if let Err(e) =
                susi_config::atomic_write_bytes(&out_path, format!("{hex}\n").as_bytes())
            {
                eprintln!("Failed to write {}: {e}", out_path.display());
                std::process::exit(1);
            }
            println!(
                "Wrote 256-bit cloud bootstrap key (0600) to `{}` (mount as SUSI_VAULT_KEY_FILE or CREDENTIALS_DIRECTORY/susi-vault-key; value not printed).",
                out_path.display()
            );
        }
        VaultCommands::Exec { client, command } => {
            match susi_config::keepass_bridge::exec_with_client_env(
                &cfg_dir,
                client.as_deref(),
                &command,
            ) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("Vault exec failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn read_secret_prompt(prompt: &str) -> io::Result<String> {
    if !io::stdin().is_terminal() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        let val = buf.trim().to_string();
        if val.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty passphrase on stdin",
            ));
        }
        return Ok(val);
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let val = line.trim().to_string();
    if val.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "passphrase must not be empty",
        ));
    }
    Ok(val)
}

// `susi_daemon::provider_ops` builds the key documents (presence only);
// the MCP `keys_*` tools return the same ones.
fn print_keys_status() {
    let status = susi_daemon::provider_ops::keys_status();
    println!("Cloud API key status (values never shown):");
    for row in status["keys"].as_array().into_iter().flatten() {
        println!(
            "  {:<12} {:<22} {}",
            row["vendor"].as_str().unwrap_or_default(),
            row["env"].as_str().unwrap_or_default(),
            if row["present"].as_bool() == Some(true) {
                "set"
            } else {
                "missing"
            }
        );
    }
    println!("\n{}", status["preference"].as_str().unwrap_or_default());
    println!(
        "\nSet:    susi keys set <vendor>\nPrefer: susi keys prefer <vendor>\nFile:   {}",
        status["cloud_env_path"].as_str().unwrap_or_default()
    );
}

fn prompt_api_key(vendor: &str) -> io::Result<String> {
    let env_hint = susi_gemi::engines::http_provider::resolve_vendor_env_name(vendor)
        .unwrap_or_else(|| "API_KEY".to_string());
    if !io::stdin().is_terminal() {
        // Piped: `printf '%s' "$DEEPSEEK_API_KEY" | susi keys set deepseek`
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        let key = buf.trim().to_string();
        if key.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty API key on stdin",
            ));
        }
        return Ok(key);
    }
    eprint!("Enter API key for {} ({}): ", vendor, env_hint);
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "API key must not be empty",
        ));
    }
    Ok(key)
}

fn run_check(vendor: Option<&str>) {
    let rows = match susi_daemon::provider_ops::keys_check(vendor) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!(
        "{:<12} {:<13} {:>7}  default model",
        "vendor", "key", "models"
    );
    for r in &rows {
        let state = serde_json::to_value(r.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let default = match r.default_model_served {
            Some(true) => format!("{} (served)", r.default_model),
            Some(false) => format!("{} (NOT served — pick another)", r.default_model),
            None => r.default_model.clone(),
        };
        println!(
            "{:<12} {:<13} {:>7}  {}",
            r.vendor, state, r.model_count, default
        );
    }
    if susi_daemon::provider_ops::any_key_rejected(&rows) {
        std::process::exit(2);
    }
}

fn run_models(vendor: &str) {
    match susi_daemon::provider_ops::keys_models(vendor) {
        Ok(rows) => {
            for r in rows.as_array().into_iter().flatten() {
                println!(
                    "{} — {} models",
                    r["vendor"].as_str().unwrap_or_default(),
                    r["model_count"]
                );
                for m in r["models"].as_array().into_iter().flatten() {
                    println!("  {}", m.as_str().unwrap_or_default());
                }
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// A new key is the operator's repair for a "no credit / rejected key"
/// quarantine: lift it so the vendor is tried again right away instead of
/// waiting out a multi-hour backoff.
fn clear_quarantine_for(vendor: &str) {
    let env = susi_gemi::engines::http_provider::resolve_vendor_env_name(vendor);
    let id = susi_gemi::engines::http_provider::known_cloud_vendors()
        .into_iter()
        .find(|(_, e)| Some(e) == env.as_ref())
        .map(|(id, _)| id)
        .unwrap_or_else(|| vendor.to_ascii_lowercase());
    if let Ok(true) =
        susi_gemi::susi_core::plane_bus::gemi::ModelManager::clear_provider_cooldown(&id)
    {
        println!("Lifted the failure quarantine on `{id}`; it will be tried again.");
    }
}
