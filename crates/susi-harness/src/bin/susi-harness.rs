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

// Retain the original Rust process entry and TLS boundary in Windows binaries.
#[cfg(windows)]
extern crate susi_windows_runtime;

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
  susi-harness adaptive [--json] [--port-offset N]
  susi-harness status  [--json] [--port-offset N]
  susi-harness models  [--verify | --discover] [--provider ID] [--ledger PATH] [--catalogue PATH]
  susi-harness budget  [--ledger PATH]
  susi-harness env
  susi-harness gateway [--port 9180] [--host 127.0.0.1] [--ledger PATH] [--catalogue PATH]
                       [--timeout SECS] [--max-output-tokens N] [--accounts PATH]
  susi-harness integrate       [--repo PATH] [--remote origin] [--include-local] [--branch PAT]
                               [--adopt-host-commit OID] [--allow-missing-trailer] [--dry-run]
                               [--max-attempts N] [--no-fetch] [--receipt-out PATH] [--json]
                               (also sweeps refs/agents/<a> as local-agent/<a>; docs/agent-refs.md)
  susi-harness submit-main     [--repo PATH] [--rev REV] [--remote origin] [--adopt-host-commit OID]
                               [--allow-missing-trailer] [--dry-run] [--max-attempts N] [--json]
  susi-harness release-promote [--repo PATH] [--remote origin] [--name RELEASE_NAME] [--candidate REV]
                               [--dry-run] [--no-fetch] [--receipt-out PATH] [--json]
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
        "adaptive" | "socket" | "status" => adaptive_command(&options),
        "models" => models_command(&options),
        "budget" => budget_command(&options),
        "env" => env_command(),
        "gateway" => gateway_command(&options),
        "integrate" | "auto-merge" => integrate_command(&options),
        "submit-main" => submit_main_command(&options),
        "release-promote" | "promote-release" => release_promote_command(&options),
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
    discover: bool,
    provider: Option<String>,
    port: u16,
    host: String,
    timeout: u64,
    max_output_tokens: Option<u32>,
    accounts: Option<PathBuf>,
    json: bool,
    port_offset: u16,
    repo: Option<PathBuf>,
    remote: String,
    include_local: bool,
    branches: Vec<String>,
    adopt_host_commits: Vec<String>,
    allow_missing_trailer: bool,
    dry_run: bool,
    max_attempts: usize,
    rev: String,
    release_name: Option<String>,
    candidate: Option<String>,
    receipt_out: Option<PathBuf>,
    no_fetch: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            workers: "auto".to_string(),
            ledger: None,
            catalogue: PathBuf::from("deploy/harness/prices.json"),
            verify: false,
            discover: false,
            provider: None,
            port: 9180,
            host: "127.0.0.1".to_string(),
            timeout: 120,
            max_output_tokens: None,
            accounts: None,
            json: false,
            port_offset: 0,
            repo: None,
            remote: "origin".to_string(),
            include_local: false,
            branches: Vec::new(),
            adopt_host_commits: Vec::new(),
            allow_missing_trailer: false,
            dry_run: false,
            max_attempts: 4,
            rev: "HEAD".to_string(),
            release_name: None,
            candidate: None,
            receipt_out: None,
            no_fetch: false,
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
                "--discover" => options.discover = true,
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
                "--json" => options.json = true,
                "--port-offset" => {
                    options.port_offset = value()?
                        .parse()
                        .map_err(|_| "--port-offset must be a number".to_string())?;
                }
                "--repo" => options.repo = Some(PathBuf::from(value()?)),
                "--remote" => options.remote = value()?,
                "--include-local" => options.include_local = true,
                "--branch" => options.branches.push(value()?),
                "--adopt-host-commit" => options.adopt_host_commits.push(value()?),
                "--allow-missing-trailer" => options.allow_missing_trailer = true,
                "--dry-run" => options.dry_run = true,
                "--max-attempts" => {
                    options.max_attempts = value()?
                        .parse()
                        .map_err(|_| "--max-attempts must be a positive integer".to_string())?;
                }
                "--rev" => options.rev = value()?,
                "--name" => options.release_name = Some(value()?),
                "--candidate" => options.candidate = Some(value()?),
                "--receipt-out" => options.receipt_out = Some(PathBuf::from(value()?)),
                "--no-fetch" => options.no_fetch = true,
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(options)
    }

    fn repo_dir(&self) -> Result<PathBuf, String> {
        match &self.repo {
            Some(path) => Ok(path.clone()),
            None => std::env::current_dir().map_err(|e| format!("current directory: {e}")),
        }
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

/// The usable models; with `--verify`, a live check of each provider; with
/// `--discover`, the chat models the keyed providers serve beyond the
/// registry, as an overlay to review.
fn models_command(options: &Options) -> Result<(), String> {
    if options.discover {
        let discovery = susi_harness::verify::discover(
            Registry::builtin(),
            options.provider.as_deref(),
            &susi_harness::verify::fetch_http,
        )
        .map_err(|error| error.to_string())?;
        for note in &discovery.notes {
            eprintln!("{note}");
        }
        return print_json(&discovery);
    }
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

/// Discovered monthly-subscription CLI tools, Susi AI OS Socket endpoints (`9090`/`9091`/`9094`),
/// active adaptive execution policy, and embedded workflow readiness.
fn adaptive_command(options: &Options) -> Result<(), String> {
    let report = susi_harness::build_adaptive_supervisor_report(options.port_offset);
    print_json(&report)
}

fn integrate_command(options: &Options) -> Result<(), String> {
    let adopted_host_commits =
        susi_harness::git_governor::parse_adopted_oids(&options.adopt_host_commits)
            .map_err(|e| e.to_string())?;
    let cfg = susi_harness::git_governor::AutoMergeConfig {
        repo: options.repo_dir()?,
        remote: options.remote.clone(),
        fetch_remote: !options.no_fetch,
        include_local: options.include_local,
        branch_filters: options.branches.clone(),
        adopted_host_commits,
        require_task_trailer: !options.allow_missing_trailer,
        dry_run: options.dry_run,
        max_attempts: options.max_attempts.max(1),
        receipt_out: options.receipt_out.clone(),
    };
    let mut note_sink = |line: String| eprintln!("{line}");
    let doc = susi_harness::operator::integrate_pending(&cfg, &mut note_sink)
        .map_err(|e| e.to_string())?;
    print_json(&doc)
}

fn submit_main_command(options: &Options) -> Result<(), String> {
    let adopted_host_commits =
        susi_harness::git_governor::parse_adopted_oids(&options.adopt_host_commits)
            .map_err(|e| e.to_string())?;
    let cfg = susi_harness::git_governor::SubmitMainConfig {
        repo: options.repo_dir()?,
        rev: options.rev.clone(),
        remote: options.remote.clone(),
        require_task_trailer: !options.allow_missing_trailer,
        adopted_host_commits,
        dry_run: options.dry_run,
        max_attempts: options.max_attempts.max(1),
    };
    let mut note_sink = |line: String| eprintln!("{line}");
    let doc =
        susi_harness::operator::submit_main(&cfg, &mut note_sink).map_err(|e| e.to_string())?;
    print_json(&doc)
}

fn release_promote_command(options: &Options) -> Result<(), String> {
    let cfg = susi_harness::git_governor::ReleasePromotionConfig {
        repo: options.repo_dir()?,
        remote: options.remote.clone(),
        fetch_remote: !options.no_fetch,
        target_release_name: options.release_name.clone(),
        candidate_rev: options.candidate.clone(),
        dry_run: options.dry_run,
        receipt_out: options.receipt_out.clone(),
    };
    let mut note_sink = |line: String| eprintln!("{line}");
    let doc =
        susi_harness::operator::promote_release(&cfg, &mut note_sink).map_err(|e| e.to_string())?;
    print_json(&doc)
}

#[cfg(test)]
fn adaptive_json(options: &Options) -> Result<String, String> {
    let report = susi_harness::build_adaptive_supervisor_report(options.port_offset);
    serde_json::to_string_pretty(&report).map_err(|error| error.to_string())
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

    #[test]
    fn adaptive_and_status_commands_report_cli_fleet_and_socket_endpoints_as_json() {
        let opts = Options::parse(&strings(&["--json", "--port-offset", "100"])).unwrap();
        assert!(opts.json);
        assert_eq!(opts.port_offset, 100);

        let text = adaptive_json(&opts).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["embedded_workflow_ready"], true);
        assert_eq!(value["socket_endpoints"]["gmcp_port"], 9190);
        assert_eq!(value["socket_endpoints"]["gemi_port"], 9191);
        assert_eq!(value["socket_endpoints"]["gawd_port"], 9194);
        assert_eq!(
            value["socket_endpoints"]["gmcp_http_url"],
            "http://127.0.0.1:9190"
        );
        assert_eq!(
            value["socket_endpoints"]["gemi_openai_url"],
            "http://127.0.0.1:9191/v1/chat/completions"
        );
        assert_eq!(
            value["socket_endpoints"]["gawd_a2a_url"],
            "http://127.0.0.1:9194"
        );

        let cli_fleet = value["cli_fleet"].as_array().unwrap();
        let kinds: Vec<&str> = cli_fleet
            .iter()
            .filter_map(|item| item["kind"].as_str())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "claude",
                "codex",
                "gemini",
                "antigravity",
                "qwen",
                "aider",
                "opencode"
            ]
        );

        assert!(run(&strings(&["adaptive", "--json"])).is_ok());
        assert!(run(&strings(&["status", "--json", "--port-offset", "10"])).is_ok());
        assert!(run(&strings(&["socket"])).is_ok());
        assert!(Options::parse(&strings(&["--port-offset", "bad"])).is_err());
    }

    #[test]
    fn git_governor_flags_parse_cleanly() {
        let opts = Options::parse(&strings(&[
            "--repo",
            "/tmp/susi-repo",
            "--remote",
            "upstream",
            "--include-local",
            "--branch",
            "local-agent/*",
            "--adopt-host-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--allow-missing-trailer",
            "--dry-run",
            "--max-attempts",
            "6",
            "--rev",
            "local-agent/test",
            "--name",
            "v0.21.56-candidate",
            "--candidate",
            "release/v0.21.56-distilled-rc11",
            "--receipt-out",
            "/tmp/receipt.json",
            "--no-fetch",
            "--json",
        ]))
        .unwrap();
        assert_eq!(opts.repo, Some(PathBuf::from("/tmp/susi-repo")));
        assert_eq!(opts.remote, "upstream");
        assert!(opts.include_local);
        assert_eq!(opts.branches, vec!["local-agent/*".to_string()]);
        assert_eq!(opts.adopt_host_commits.len(), 1);
        assert!(opts.allow_missing_trailer);
        assert!(opts.dry_run);
        assert_eq!(opts.max_attempts, 6);
        assert_eq!(opts.rev, "local-agent/test");
        assert_eq!(opts.release_name.as_deref(), Some("v0.21.56-candidate"));
        assert_eq!(
            opts.candidate.as_deref(),
            Some("release/v0.21.56-distilled-rc11")
        );
        assert_eq!(opts.receipt_out, Some(PathBuf::from("/tmp/receipt.json")));
        assert!(opts.no_fetch);
        assert!(Options::parse(&strings(&["--max-attempts", "bad"])).is_err());
    }
}
