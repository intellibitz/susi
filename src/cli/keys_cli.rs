//! `susi keys` — cloud API key lifecycle (set / list / prefer / remove).
//! Never prints secret material; `set` accepts a piped key on stdin or prompts.

use super::defs::KeyCommands;

use std::io::{self, IsTerminal, Read, Write};

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
    }
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
