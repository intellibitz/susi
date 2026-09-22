//! `susi keys` — cloud API key lifecycle (set / list / prefer / remove).
//! Never prints secret material; `set` accepts a piped key on stdin or prompts.

use crate::cli_defs::KeyCommands;

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
            match susi_gemi::http_provider::register_api_key(&vendor, &key) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Key registration failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(KeyCommands::Prefer { vendor, clear }) => {
            if clear {
                match susi_gemi::routing::InferenceRouter::clear_preferred_cloud() {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                }
            } else if let Some(v) = vendor {
                match susi_gemi::routing::InferenceRouter::set_preferred_cloud(&v) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                println!(
                    "{}",
                    susi_gemi::routing::InferenceRouter::preference_status()
                );
            }
        }
        Some(KeyCommands::Remove { vendor }) => {
            match susi_gemi::http_provider::remove_api_key(&vendor) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Key removal failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}

fn print_keys_status() {
    println!("Cloud API key status (values never shown):");
    for (vendor, env, present) in susi_gemi::http_provider::list_api_key_status() {
        println!(
            "  {:<12} {:<22} {}",
            vendor,
            env,
            if present { "set" } else { "missing" }
        );
    }
    println!(
        "\n{}",
        susi_gemi::routing::InferenceRouter::preference_status()
    );
    println!(
        "\nSet:    susi keys set <vendor>\nPrefer: susi keys prefer <vendor>\nFile:   {}",
        susi_gemi::http_provider::cloud_env_path().display()
    );
}

fn prompt_api_key(vendor: &str) -> io::Result<String> {
    let env_hint = susi_gemi::http_provider::resolve_vendor_env_name(vendor)
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
