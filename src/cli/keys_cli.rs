//! `susi keys` â€” cloud API key lifecycle (set / list / prefer / remove).
//! Never prints secret material; `set` accepts a piped key on stdin or prompts.

use super::defs::{KeyCommands, VaultCommands};

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

pub(crate) fn run(action: Option<KeyCommands>) {
    match action {
        Some(KeyCommands::Vault { action }) => run_vault(action),
        _ => {
            if let Err(error) = super::provider_forward::execute(None) {
                eprintln!("{}", susi_config::redact_credentials(&error.to_string()));
                std::process::exit(1);
            }
        }
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
                    "WARNING:          {} plaintext key(s) remain in cloud.env â€” run `susi keys vault migrate-cloud-env`",
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
                if susi_config::vault_exec::is_personal_vault_path(db_path) {
                    eprintln!(
                        "Refusing `{}`: Personal KeePassXC vaults are off limits.",
                        db_path.display()
                    );
                    std::process::exit(1);
                }
            }
            if let Some(ref gd_path) = gdrive_path {
                if susi_config::vault_exec::is_personal_vault_path(gd_path) {
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
            let mut args = vec!["sync".to_string(), "--direction".to_string(), direction];
            push_database(&mut args, db, "Vault sync failed");
            push_flag(&mut args, dry_run, "--dry-run");
            push_flag(&mut args, json, "--json");
            std::process::exit(forward_vault(&args, &[]));
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
            refresh_sealed_vault();
            match susi_config::vault_exec::exec_with_client_env(
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
        VaultCommands::Doctor { json } => {
            let mut args = vec!["doctor".to_string()];
            push_flag(&mut args, json, "--json");
            std::process::exit(forward_vault(&args, &[]));
        }
        VaultCommands::PrimeSigning { gpg_only, json } => {
            let mut args = vec!["prime-signing".to_string()];
            push_flag(&mut args, gpg_only, "--gpg-only");
            push_flag(&mut args, json, "--json");
            std::process::exit(forward_vault(&args, &[]));
        }
        VaultCommands::SetupGit { gpgsign, json } => {
            let mut args = vec!["setup-git".to_string()];
            push_flag(&mut args, gpgsign, "--gpgsign");
            push_flag(&mut args, json, "--json");
            std::process::exit(forward_vault(&args, &[]));
        }
        VaultCommands::Rekey {
            db,
            key_file_out,
            no_key_file,
            with_passphrase,
            json,
        } => {
            let mut args = vec!["rekey".to_string()];
            push_database(&mut args, db, "Rekey failed");
            if let Some(out) = key_file_out {
                args.push("--key-file-out".to_string());
                args.push(out.to_string_lossy().into_owned());
            }
            push_flag(&mut args, no_key_file, "--no-key-file");
            push_flag(&mut args, with_passphrase, "--with-passphrase");
            push_flag(&mut args, json, "--json");
            // Core asked for the new passphrase at a terminal; `susi-commercial` reads it
            // from the environment or piped stdin only, so the prompt stays here and the
            // value reaches the child's environment alone.
            let mut prompted = None;
            if std::env::var_os("SUSI_NEW_KDBX_PASSPHRASE").is_none()
                && (with_passphrase || no_key_file)
                && io::stdin().is_terminal()
            {
                match read_secret_prompt(
                    "Enter new portable KeePassXC passphrase (piped stdin supported): ",
                ) {
                    Ok(p) => prompted = Some(p),
                    Err(e) => {
                        eprintln!("Failed to read new passphrase: {e}");
                        std::process::exit(1);
                    }
                }
            }
            let envs: Vec<(&str, &str)> = prompted
                .as_deref()
                .map(|p| vec![("SUSI_NEW_KDBX_PASSPHRASE", p)])
                .unwrap_or_default();
            let code = forward_vault(&args, &envs);
            if let Some(mut p) = prompted {
                susi_config::vault_store::wipe_string(&mut p);
            }
            std::process::exit(code);
        }
        VaultCommands::GpgProgram { args } => {
            std::process::exit(susi_config::vault_exec::run_git_gpg_program(&args));
        }
    }
}

/// Runs a KeePassXC verb in `susi-commercial vault`, which serves the bridge that left core
/// (Mandate 84); prints the command to run and exits 2 when it is not installed.
fn forward_vault(args: &[String], envs: &[(&str, &str)]) -> i32 {
    match susi_config::vault_exec::run_commercial_vault(args, envs) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

fn push_flag(args: &mut Vec<String>, on: bool, flag: &str) {
    if on {
        args.push(flag.to_string());
    }
}

/// `--db` with core's precedence (explicit, then `SUSI_VAULT_DB`, then `SUSI_COMMERCIAL_VAULT`).
fn push_database(args: &mut Vec<String>, explicit: Option<PathBuf>, failure: &str) {
    match susi_config::vault_exec::forwarded_database(explicit) {
        Ok(Some(db)) => {
            args.push("--db".to_string());
            args.push(db.to_string_lossy().into_owned());
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("{failure}: {e}");
            std::process::exit(1);
        }
    }
}

/// Core's bridge refreshed the sealed vault from the enrolled KeePassXC database before
/// `exec` read it. Susi Commercial Apps serves that refresh now: its plug host over the
/// plane bus, else the installed `susi-commercial`. Best effort: `exec` reads what is sealed.
fn refresh_sealed_vault() {
    let topic = susi_config::vault_exec::TOPIC_KDBX_AUTO_SYNC;
    let bus = susi_core::plane_bus::PlaneBus::global();
    if bus.is_wired(topic) {
        let _ = bus.request(topic, serde_json::json!({}));
        return;
    }
    let _ = susi_config::vault_exec::commercial_auto_sync();
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
    if susi_paths::console::is_unattended_env() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unattended mode forbids interactive passphrase prompt; use piped stdin, SUSI_VAULT_KEY, systemd-creds, or secret-tool",
        ));
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
