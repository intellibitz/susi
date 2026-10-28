#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! `susi-harness`: the harness on its own.
//!
//! It needs nothing from the rest of susi: the provider registry and the price
//! catalogue are compiled in, keys come from the environment, and the ledger
//! is a file. When nothing else in susi works, this still meters, budgets
//! and routes model calls.
//!
//! ```text
//! susi-harness fleet                  the orchestrator and ladder the keys give
//! susi-harness models [--verify]      usable models, or check them live
//! susi-harness budget                 the budget line and any alerts
//! susi-harness env                    every variable it reads (keys never shown)
//! susi-harness gateway [--port 9180]  the metering gateway, OpenAI-compatible
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::fleet;
use susi_harness::gateway::GatewayState;
use susi_harness::providers::Registry;

const USAGE: &str = "susi-harness: model routing, metering and budgets for agent fleets

usage:
  susi-harness fleet   [--workers auto|<m1,m2,...>] [--ledger PATH] [--catalogue PATH]
  susi-harness models  [--verify [--provider ID]] [--ledger PATH] [--catalogue PATH]
  susi-harness budget  [--ledger PATH]
  susi-harness env
  susi-harness gateway [--port 9180] [--host 127.0.0.1] [--ledger PATH] [--catalogue PATH]
                       [--timeout SECS] [--max-output-tokens N] [--accounts PATH]
  susi-harness version

defaults: --ledger out/ledger.jsonl, --catalogue deploy/harness/prices.json (the
built-in catalogue when that file does not exist). Keys and settings are read
from the process, user and machine environment; `susi-harness env` lists them.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = run(&args) {
        eprintln!("susi-harness: {message}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(());
    };
    // Fail closed on a registry overlay the operator named but cannot be used.
    if let Some(error) = Registry::overlay_error() {
        return Err(error);
    }
    let options = Options::parse(rest)?;
    match command.as_str() {
        "fleet" => fleet_command(&options),
        "models" => models_command(&options),
        "budget" => budget_command(&options),
        "env" => env_command(),
        "gateway" => gateway_command(&options),
        "version" | "--version" | "-V" => {
            println!("susi-harness {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

/// Every flag any command takes; each command reads the ones it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    workers: String,
    ledger: PathBuf,
    catalogue: PathBuf,
    verify: bool,
    provider: Option<String>,
    port: u16,
    host: String,
    timeout: u64,
    max_output_tokens: Option<u32>,
    accounts: Option<PathBuf>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            workers: "auto".to_string(),
            ledger: PathBuf::from("out/ledger.jsonl"),
            catalogue: PathBuf::from("deploy/harness/prices.json"),
            verify: false,
            provider: None,
            port: 9180,
            host: "127.0.0.1".to_string(),
            timeout: 120,
            max_output_tokens: None,
            accounts: None,
        };
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match flag.as_str() {
                "--workers" => options.workers = value()?,
                "--ledger" => options.ledger = PathBuf::from(value()?),
                "--catalogue" => options.catalogue = PathBuf::from(value()?),
                "--verify" => options.verify = true,
                "--provider" => options.provider = Some(value()?),
                "--port" => {
                    options.port = value()?
                        .parse()
                        .map_err(|_| "--port must be a number".to_string())?;
                }
                "--host" => options.host = value()?,
                "--timeout" => {
                    options.timeout = value()?
                        .parse()
                        .map_err(|_| "--timeout must be a number of seconds".to_string())?;
                }
                "--max-output-tokens" => {
                    options.max_output_tokens = Some(
                        value()?
                            .parse()
                            .map_err(|_| "--max-output-tokens must be a number".to_string())?,
                    );
                }
                "--accounts" => options.accounts = Some(PathBuf::from(value()?)),
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(options)
    }

    fn catalogue(&self) -> Result<PriceCatalogue, String> {
        PriceCatalogue::load_or_builtin(&self.catalogue).map_err(|error| error.to_string())
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

fn alert(budgets: &susi_harness::balance::Budgets) {
    for line in fleet::alerts(budgets) {
        eprintln!("{line}");
    }
}

/// The fleet the keys give: orchestrator and ladder, with the alerts.
fn fleet_command(options: &Options) -> Result<(), String> {
    let (budgets, _) = fleet::gather_budgets(&options.ledger);
    alert(&budgets);
    let resolved = fleet::resolve_fleet(
        &options.workers,
        || PriceCatalogue::load_or_builtin(&options.catalogue),
        &budgets,
        &fleet::measured_latency(&options.ledger),
    )
    .map_err(|error| error.to_string())?;
    for note in &resolved.left_off {
        eprintln!("⚠ {note}");
    }
    print_json(&serde_json::json!({
        "orchestrator": resolved.orchestrator,
        "ladder": resolved.ladder,
    }))
}

/// The usable models, or, with `--verify`, a live check of each provider.
fn models_command(options: &Options) -> Result<(), String> {
    if options.verify {
        let report = susi_harness::verify::verify_registry(
            Registry::builtin(),
            options.provider.as_deref(),
            &susi_harness::verify::fetch_http,
        )
        .map_err(|error| error.to_string())?;
        print_json(&report)?;
        if report.ok {
            return Ok(());
        }
        let failed = report
            .providers
            .iter()
            .filter(|entry| !entry.ok)
            .map(|entry| entry.provider.as_str())
            .collect::<Vec<_>>();
        return Err(format!("verification failed for: {}", failed.join(", ")));
    }
    let catalogue = options.catalogue()?;
    let (budgets, _) = fleet::gather_budgets(&options.ledger);
    let report = fleet::available_report(
        Some(&catalogue),
        &budgets,
        &fleet::measured_latency(&options.ledger),
    );
    print_json(&report)
}

/// The one-line budget report, and any alerts.
fn budget_command(options: &Options) -> Result<(), String> {
    let (budgets, spent) = fleet::gather_budgets(&options.ledger);
    println!(
        "{}",
        budgets.report_line(fleet::total_spent(&spent), Registry::builtin())
    );
    alert(&budgets);
    Ok(())
}

/// Every variable the harness reads; keys are reported as set, never shown.
fn env_command() -> Result<(), String> {
    print_json(&fleet::environment())
}

/// The metering gateway: OpenAI-compatible, routing each model to its
/// provider with that provider's key.
fn gateway_command(options: &Options) -> Result<(), String> {
    let catalogue = options.catalogue()?;
    if let Some(parent) = options
        .ledger
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let cache_dir = options.ledger.parent().map(Path::to_path_buf);
    let state = Arc::new(GatewayState {
        catalogue,
        ledger_path: options.ledger.clone(),
        upstream_chat_url: "direct".to_string(),
        provider: "susi-harness".to_string(),
        api_key: None,
        timeout_secs: options.timeout,
        max_output_tokens: options.max_output_tokens,
        cache: Some(Arc::new(susi_harness::cache::ResponseCache::new(cache_dir))),
    });
    let ingress = match &options.accounts {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("accounts {}: {error}", path.display()))?;
            Some(Arc::new(
                susi_harness::ingress::Ingress::parse(&text).map_err(|error| error.to_string())?,
            ))
        }
        None => None,
    };
    if ingress.is_none() && !is_loopback(&options.host) {
        return Err(format!(
            "--host {} is reachable from other machines; pass --accounts so callers must authenticate",
            options.host
        ));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("runtime: {error}"))?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((options.host.as_str(), options.port))
            .await
            .map_err(|error| format!("bind {}:{}: {error}", options.host, options.port))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("local address: {error}"))?;
        eprintln!(
            "susi-harness gateway listening on http://{address}/v1 (ledger {})",
            options.ledger.display()
        );
        match ingress {
            Some(ingress) => susi_harness::ingress::serve(listener, state, ingress).await,
            None => susi_harness::gateway::serve(listener, state).await,
        }
        .map_err(|error| format!("serve: {error}"))
    })
}

/// Whether `host` only accepts connections from this machine.
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn defaults_need_no_flags() {
        let options = Options::parse(&[]).unwrap();
        assert_eq!(options.workers, "auto");
        assert_eq!(options.port, 9180);
        assert_eq!(options.host, "127.0.0.1");
        assert_eq!(options.ledger, PathBuf::from("out/ledger.jsonl"));
    }

    #[test]
    fn flags_are_read_and_bad_ones_refused() {
        let options = Options::parse(&strings(&[
            "--workers",
            "deepseek-flash,claude-haiku",
            "--port",
            "9300",
            "--verify",
            "--provider",
            "anthropic",
        ]))
        .unwrap();
        assert_eq!(options.workers, "deepseek-flash,claude-haiku");
        assert_eq!(options.port, 9300);
        assert!(options.verify);
        assert_eq!(options.provider.as_deref(), Some("anthropic"));
        assert!(Options::parse(&strings(&["--port", "many"])).is_err());
        assert!(Options::parse(&strings(&["--ledger"])).is_err());
        assert!(Options::parse(&strings(&["--nope"])).is_err());
    }

    #[test]
    fn an_unknown_command_is_an_error_and_no_command_prints_usage() {
        assert!(run(&strings(&["launch"])).is_err());
        assert!(run(&[]).is_ok());
        assert!(run(&strings(&["version"])).is_ok());
    }

    #[test]
    fn only_loopback_hosts_may_serve_without_accounts() {
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("localhost"));
        assert!(!is_loopback("0.0.0.0"));
        assert!(!is_loopback("192.168.1.10"));
    }
}
