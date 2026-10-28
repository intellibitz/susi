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
//! susi-harness                        zero config: find the keys, report, serve
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
use susi_harness::ingress::Ingress;
use susi_harness::providers::{ProviderRecord, Registry};

const USAGE: &str = "susi-harness: model routing, metering and budgets for agent fleets

usage:
  susi-harness                         find your keys, show the fleet and budget, start the gateway
  susi-harness up      [gateway flags]  the same, by name
  susi-harness fleet   [--workers auto|<m1,m2,...>] [--ledger PATH] [--catalogue PATH]
  susi-harness models  [--verify [--provider ID]] [--ledger PATH] [--catalogue PATH]
  susi-harness budget  [--ledger PATH]
  susi-harness env
  susi-harness gateway [--port 9180] [--host 127.0.0.1] [--ledger PATH] [--catalogue PATH]
                       [--timeout SECS] [--max-output-tokens N] [--accounts PATH]
  susi-harness version

Bring your own key: set any provider's key in the environment or in a .env file
(here, or in ~/.susi/.env). One key is enough; `susi-harness env` lists them all.

defaults: --ledger is out/ledger.jsonl in a susi workspace, else
~/.susi/harness/ledger.jsonl (SUSI_HOME moves ~/.susi); --catalogue is
deploy/harness/prices.json when it exists, else the built-in catalogue.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = run(&args) {
        eprintln!("susi-harness: {message}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    // Help and version answer first, ahead of anything that can fail: with
    // flags meaning `up`, `susi-harness --help` read as `up --help` and
    // exited 1 on an unknown argument.
    let first = args.first().map(String::as_str);
    if first == Some("help") || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    if matches!(first, Some("version" | "--version" | "-V")) {
        println!("susi-harness {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // No command, or only flags (`susi-harness --port 9300`), means `up`.
    let (command, rest) = match args.split_first() {
        Some((command, rest)) if !command.starts_with("--") => (command.as_str(), rest),
        _ => ("up", args),
    };
    // Fail closed on a registry overlay the operator named but cannot be used.
    if let Some(error) = Registry::overlay_error() {
        return Err(error);
    }
    let options = Options::parse(rest)?;
    match command {
        "up" => up_command(&options),
        "fleet" => fleet_command(&options),
        "models" => models_command(&options),
        "budget" => budget_command(&options),
        "env" => env_command(),
        "gateway" => gateway_command(&options),
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

/// Every flag any command takes; each command reads the ones it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    workers: String,
    /// `None` means the default ledger ([`fleet::default_ledger`]).
    ledger: Option<PathBuf>,
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
            ledger: None,
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
                "--ledger" => options.ledger = Some(PathBuf::from(value()?)),
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

    fn ledger(&self) -> PathBuf {
        self.ledger.clone().unwrap_or_else(|| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            fleet::default_ledger(&cwd)
        })
    }

    fn catalogue(&self) -> Result<PriceCatalogue, String> {
        PriceCatalogue::load_or_builtin(&self.catalogue).map_err(|error| error.to_string())
    }
}

/// What the real calls say about the models: the source of truth for which
/// ones are active. Models whose last call was refused long enough ago are
/// probed with one real call each, and each probe is reported
/// ([`fleet::probe_report`]).
fn evidence(options: &Options, ledger: &Path) -> fleet::Evidence {
    let catalogue = options.catalogue().ok();
    let evidence = fleet::evidence(ledger, catalogue.as_ref(), true);
    for line in fleet::probe_report(&evidence) {
        eprintln!("{line}");
    }
    if !evidence.failed.is_empty() {
        let names = evidence.failed.keys().cloned().collect::<Vec<_>>();
        eprintln!("inactive (last real call refused): {}", names.join(", "));
    }
    evidence
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

/// Zero config: find the keys, say what they give, and serve. With no key,
/// say where to get one.
fn up_command(options: &Options) -> Result<(), String> {
    println!("susi-harness {}", env!("CARGO_PKG_VERSION"));
    let (keyed, _) = fleet::providers_by_key();
    // A local provider (no `key_env`) serves with no key at all: counting
    // only keyed providers told an Ollama-only user "No API key found" and
    // exited without serving the fleet `resolve_fleet` would have built.
    let local = local_providers(Registry::builtin());
    if keyed.is_empty() && local.is_empty() {
        println!("{}", fleet::onboarding());
        return Ok(());
    }
    // Refuse an exposed host before probing models or printing a fleet.
    let ingress = load_ingress(options)?;
    if !keyed.is_empty() {
        println!("keys:   {}", provider_ids(&keyed));
    }
    if !local.is_empty() {
        println!("local:  {} (no key)", provider_ids(&local));
    }
    let ledger = options.ledger();
    println!("ledger: {}", ledger.display());
    let (budgets, spent) = fleet::gather_budgets(&ledger);
    alert(&budgets);
    let resolved = fleet::resolve_fleet(
        &options.workers,
        || PriceCatalogue::load_or_builtin(&options.catalogue),
        &budgets,
        &evidence(options, &ledger),
    )
    .map_err(|error| error.to_string())?;
    println!(
        "fleet:  {} orchestrates; workers {}; fixers {}",
        resolved.orchestrator.as_deref().unwrap_or("none"),
        resolved.workers.join(", "),
        resolved.fixers.join(", ")
    );
    println!(
        "{}",
        budgets.report_line(fleet::total_spent(&spent), Registry::builtin())
    );
    println!(
        "use:    point any OpenAI-compatible client at http://{}:{}/v1 and pick a model above",
        options.host, options.port
    );
    serve_gateway(options, ingress)
}

/// The providers that need no key: an overlay's local servers.
fn local_providers(registry: &Registry) -> Vec<&ProviderRecord> {
    registry
        .providers()
        .iter()
        .filter(|provider| provider.key_envs.is_empty() && !provider.models.is_empty())
        .collect()
}

fn provider_ids(providers: &[&ProviderRecord]) -> String {
    providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The fleet the keys give: orchestrator and ladder, with the alerts.
fn fleet_command(options: &Options) -> Result<(), String> {
    let (budgets, _) = fleet::gather_budgets(&options.ledger());
    alert(&budgets);
    let resolved = fleet::resolve_fleet(
        &options.workers,
        || PriceCatalogue::load_or_builtin(&options.catalogue),
        &budgets,
        &evidence(options, &options.ledger()),
    )
    .map_err(|error| error.to_string())?;
    for note in &resolved.left_off {
        eprintln!("⚠ {note}");
    }
    // For each light worker, the order a task that starts on it follows: its
    // own model, then the strong fixers, then the other light models, as the
    // adapter computes it from the same ledger.
    let entries = susi_harness::ledger::read_entries_lenient(&options.ledger()).0;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let registry = Registry::builtin();
    let strong =
        |model: &str| registry.tier_of(model) == Some(susi_harness::providers::Tier::Rescue);
    let ladder = susi_harness::policy::escalation_order(&resolved.ladder, &strong);
    let history = susi_harness::policy::history(&entries);
    let cooling = susi_harness::policy::cooling_providers(&entries, now_ms);
    let failed =
        susi_harness::policy::in_force(susi_harness::policy::failed_models(&entries), now_ms);
    let standing = susi_harness::policy::Standing {
        history: &history,
        cooling: &cooling,
        failed: &failed,
    };
    let provider_of = |model: &str| registry.resolve(model).map(|resolved| resolved.provider);
    let escalation = resolved
        .workers
        .iter()
        .map(|worker| {
            (
                worker.clone(),
                susi_harness::policy::arrange(worker, &ladder, &standing, &provider_of),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    print_json(&serde_json::json!({
        "orchestrator": resolved.orchestrator,
        "workers": resolved.workers,
        "fixers": resolved.fixers,
        "ladder": resolved.ladder,
        "escalation": escalation,
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
        // A provider with no key cannot be verified and is not in use; it is
        // reported as skipped. Every keyed provider must pass, and so must a
        // provider named with --provider, keyed or not.
        let keyed = |id: &str| {
            Registry::builtin()
                .providers()
                .iter()
                .find(|provider| provider.id == id)
                .is_some_and(|provider| {
                    provider
                        .key_envs
                        .iter()
                        .any(|name| susi_harness::secrets::is_set(name))
                })
        };
        let judged = |id: &str| options.provider.is_some() || keyed(id);
        let skipped = report
            .providers
            .iter()
            .filter(|entry| !judged(&entry.provider))
            .map(|entry| entry.provider.as_str())
            .collect::<Vec<_>>();
        if !skipped.is_empty() {
            eprintln!("skipped (no key): {}", skipped.join(", "));
        }
        let failed = report
            .providers
            .iter()
            .filter(|entry| !entry.ok && judged(&entry.provider))
            .map(|entry| entry.provider.as_str())
            .collect::<Vec<_>>();
        if failed.is_empty() {
            return Ok(());
        }
        return Err(format!("verification failed for: {}", failed.join(", ")));
    }
    let catalogue = options.catalogue()?;
    let (budgets, _) = fleet::gather_budgets(&options.ledger());
    let report = fleet::available_report(
        Some(&catalogue),
        &budgets,
        &evidence(options, &options.ledger()),
    );
    print_json(&report)
}

/// The one-line budget report, and any alerts.
fn budget_command(options: &Options) -> Result<(), String> {
    let (budgets, spent) = fleet::gather_budgets(&options.ledger());
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
    let ingress = load_ingress(options)?;
    serve_gateway(options, ingress)
}

/// The `--accounts` file, if any, once the serving rule allows `--host`
/// with it: an address other machines can reach needs at least one account.
fn load_ingress(options: &Options) -> Result<Option<Arc<Ingress>>, String> {
    let ingress = match &options.accounts {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("accounts {}: {error}", path.display()))?;
            Some(Arc::new(
                Ingress::parse(&text).map_err(|error| error.to_string())?,
            ))
        }
        None => None,
    };
    susi_harness::ingress::require_accounts_off_loopback(&options.host, ingress.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(ingress)
}

fn serve_gateway(options: &Options, ingress: Option<Arc<Ingress>>) -> Result<(), String> {
    let catalogue = options.catalogue()?;
    let ledger = options.ledger();
    if let Some(parent) = ledger
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let cache_dir = ledger.parent().map(Path::to_path_buf);
    let state = Arc::new(GatewayState {
        catalogue,
        ledger_path: ledger.clone(),
        upstream_chat_url: "direct".to_string(),
        provider: "susi-harness".to_string(),
        api_key: None,
        timeout_secs: options.timeout,
        max_output_tokens: options.max_output_tokens,
        cache: Some(Arc::new(susi_harness::cache::ResponseCache::new(cache_dir))),
    });
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
            ledger.display()
        );
        match ingress {
            Some(ingress) => susi_harness::ingress::serve(listener, state, ingress).await,
            None => susi_harness::gateway::serve(listener, state).await,
        }
        .map_err(|error| format!("serve: {error}"))
    })
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
        assert_eq!(
            options.ledger, None,
            "the default ledger is resolved at use"
        );
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
        assert!(run(&strings(&["help"])).is_ok());
        // Flags alone are `up`'s flags, not a command.
        assert!(run(&strings(&["--port", "not-a-port"]))
            .unwrap_err()
            .contains("--port must be a number"));
        assert!(run(&strings(&["version"])).is_ok());
    }

    /// The flags a newcomer types first. With flags meaning `up`, `--help`
    /// and `--version` were read as gateway flags and exited 1.
    #[test]
    fn help_and_version_answer_as_flags_too() {
        for line in [
            &["--help"][..],
            &["-h"],
            &["gateway", "--help"],
            &["fleet", "-h"],
            &["--version"],
            &["-V"],
        ] {
            assert!(run(&strings(line)).is_ok(), "{line:?}");
        }
    }

    fn options_with(host: &str, accounts: Option<&Path>) -> Options {
        let mut options = Options::parse(&strings(&["--host", host])).unwrap();
        options.accounts = accounts.map(Path::to_path_buf);
        options
    }

    #[test]
    fn only_loopback_hosts_may_serve_without_accounts() {
        assert!(load_ingress(&options_with("127.0.0.1", None))
            .unwrap()
            .is_none());
        assert!(load_ingress(&options_with("localhost", None)).is_ok());
        for host in ["0.0.0.0", "::", "192.168.1.10"] {
            let error = load_ingress(&options_with(host, None)).unwrap_err();
            assert!(error.contains("--accounts"), "{error}");
        }
    }

    /// `{"accounts": []}` passed the check (an accounts file was given), and
    /// the ingress then admitted every caller, without a token, uncapped.
    #[test]
    fn an_accounts_file_with_no_account_does_not_open_a_public_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, r#"{"accounts": []}"#).unwrap();
        let error = load_ingress(&options_with("0.0.0.0", Some(&empty))).unwrap_err();
        assert!(error.contains("lists no account"), "{error}");
        let one = dir.path().join("one.json");
        std::fs::write(
            &one,
            r#"{"accounts": [{"token": "sk-a", "budget_usd": "1"}]}"#,
        )
        .unwrap();
        let ingress = load_ingress(&options_with("0.0.0.0", Some(&one)))
            .unwrap()
            .unwrap();
        assert!(!ingress.is_open());
    }

    /// The README's own overlay example: one local provider, no `key_env`.
    /// `up` counted only keyed providers, so this alone printed "No API key
    /// found" and exited without serving.
    #[test]
    fn a_keyless_local_provider_is_enough_to_serve() {
        let overlay = Registry::parse(
            r#"{"providers": [{
                "id": "local",
                "wire": "openai",
                "endpoint": "http://127.0.0.1:8080/v1/chat/completions",
                "models": [
                    {"id": "local-coder", "context": 32768, "tools": true, "tier": "worker"}
                ]
            }]}"#,
        )
        .unwrap();
        assert_eq!(provider_ids(&local_providers(&overlay)), "local");
        let keyed = Registry::parse(
            r#"{"providers": [{
                "id": "deepseek",
                "wire": "openai",
                "endpoint": "https://api.deepseek.com/chat/completions",
                "key_env": ["DEEPSEEK_API_KEY"],
                "models": [{"id": "deepseek-flash", "tier": "worker"}]
            }]}"#,
        )
        .unwrap();
        assert!(local_providers(&keyed).is_empty());
    }
}
