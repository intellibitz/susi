//! `susi harness`: author a plan, run a mission, serve the harness API — the
//! orchestration surfaces driven from the susi binary itself.
//!
//! The gateway stays the meter. These commands call the same library the
//! `harness-*` binaries do, so the mission loop lives inside susi rather than
//! beside it.

use crate::anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
pub use susi_cli_defs::harness::HarnessCommands;
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::cockpit::{serve, Cockpit, CommandAuthContext, CommandAuthorizer};
use susi_harness::fleet;
use susi_harness::money::MicroUsd;
use susi_harness::operator::{self, ModelsMode};
use susi_harness::providers::Registry;

/// Run one `susi harness` command.
///
/// `plan`, `author`, `run`, `status`, `budget` and `models` call
/// [`susi_harness::operator`], the library the MCP `harness_*` tools call
/// too; operator notes go to stderr as they happen.
///
/// # Errors
///
/// Returns the first error from authoring, validation, execution or I/O.
pub fn execute(action: HarnessCommands, cwd: &Path) -> Result<()> {
    // Fail closed: an overlay the operator named but the harness cannot use
    // would otherwise run every command on a registry they did not ask for.
    if let Some(error) = Registry::overlay_error() {
        bail!("{error}");
    }
    let mut notes = |line: String| eprintln!("{line}");
    match action {
        HarnessCommands::Budget { ledger } => {
            let doc = operator::budget(
                &ledger.unwrap_or_else(|| fleet::default_ledger(cwd)),
                cwd,
                &mut |_: String| {},
            );
            println!("{}", doc["report"].as_str().unwrap_or_default());
            if let Some(roi) = doc["roi"].as_str() {
                println!("ROI: {roi}");
            }
            for line in doc["alerts"].as_array().into_iter().flatten() {
                eprintln!("{}", line.as_str().unwrap_or_default());
            }
            Ok(())
        }
        HarnessCommands::Models {
            verify,
            discover,
            provider,
            available,
        } => {
            let mode = if discover {
                ModelsMode::Discover
            } else if verify {
                ModelsMode::Verify
            } else if available {
                ModelsMode::Available
            } else {
                ModelsMode::Registry
            };
            let doc = operator::models(mode, provider.as_deref(), cwd, &mut notes)?;
            println!("{}", serde_json::to_string_pretty(&doc)?);
            let failed = if mode == ModelsMode::Verify {
                operator::verify_failures(&doc)
            } else {
                Vec::new()
            };
            if !failed.is_empty() {
                // The report is printed even when it fails, then the exit is
                // non-zero so a caller can gate on drift without parsing it.
                bail!("model verification failed for: {}", failed.join(", "));
            }
            Ok(())
        }
        HarnessCommands::Gateway {
            port,
            host,
            catalogue,
            ledger,
            upstream,
            timeout,
            accounts,
        } => serve_gateway(
            port,
            &host,
            catalogue,
            ledger.unwrap_or_else(|| fleet::default_ledger(cwd)),
            upstream,
            timeout,
            accounts,
            cwd,
        ),
        HarnessCommands::Status { ledger, events } => {
            let snapshot = operator::status(ledger, events, None, None)?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
        HarnessCommands::Api {
            ledger,
            events,
            state,
            plan,
            controls,
            catalogue,
            graph,
            budget_usd,
            mission,
            host,
            port,
        } => serve_api(
            ledger, events, state, plan, controls, catalogue, graph, budget_usd, mission, &host,
            port,
        ),
        HarnessCommands::Author {
            gateway,
            model,
            roadmap,
            vector,
            out,
            max_tasks,
            workers,
        } => {
            let opts = operator::AuthorOptions {
                gateway,
                model,
                roadmap,
                vector,
                out,
                max_tasks,
                workers,
            };
            let doc = operator::author(&opts, cwd, &mut notes)?;
            println!("{}", doc["message"].as_str().unwrap_or_default());
            Ok(())
        }
        HarnessCommands::PlanQueue {
            repo,
            out,
            mission,
            limit,
            workers,
            roadmap,
            gateway,
        } => {
            let opts = operator::PlanQueueOptions {
                repo,
                out,
                mission,
                limit,
                workers,
                roadmap,
                gateway,
            };
            let doc = operator::plan_queue(&opts, cwd, &mut notes)?;
            println!("{}", doc["message"].as_str().unwrap_or_default());
            Ok(())
        }
        HarnessCommands::Run {
            plan,
            repo,
            state,
            gateway,
            model,
            timeout,
            parallel,
            prefix,
            workers,
            budget_usd,
            catalogue,
            daily_usd,
            ledger,
            controls,
            rsi_log,
            attempts,
            adapter_state,
            susi,
        } => {
            let opts = operator::RunOptions {
                plan,
                repo,
                state,
                gateway,
                model,
                timeout,
                parallel,
                prefix,
                workers,
                budget_usd,
                catalogue,
                daily_usd,
                ledger: ledger.unwrap_or_else(|| fleet::default_ledger(cwd)),
                controls,
                rsi_log,
                attempts,
                adapter_state,
                susi: susi.unwrap_or_else(|| {
                    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("susi"))
                }),
            };
            let outcome = operator::run(&opts, cwd, &mut notes);
            // The report prints first, then the budget line, whether or not
            // the run succeeded.
            let result = outcome
                .report
                .map_err(crate::anyhow::Error::from)
                .and_then(|report| {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    match operator::mission_failure(&report) {
                        Some(failure) => Err(crate::anyhow::anyhow!(failure)),
                        None => Ok(()),
                    }
                });
            println!("{}", outcome.budget_line);
            result
        }
        HarnessCommands::Scorecard {
            events,
            task,
            json,
            require_complete,
            python,
        } => scorecard(&events, &task, json, require_complete, &python, cwd),
    }
}

/// Run the repository's scorecard against the lifecycle ledger.
///
/// The script owns the report format; this command makes it reachable from the
/// binary, so a task's loop is measured by a caller instead of by a human who
/// remembers to run a script.
#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn scorecard(
    events: &Path,
    tasks: &[String],
    json: bool,
    require_complete: bool,
    python: &Path,
    cwd: &Path,
) -> Result<()> {
    let mut command = std::process::Command::new(python);
    command
        .arg("scripts/check-rsi-scorecard.py")
        .arg("--events")
        .arg(events);
    for task in tasks {
        command.arg("--task").arg(task);
    }
    if json {
        command.arg("--json");
    }
    if require_complete {
        command.arg("--require-complete");
    }
    let status = command
        .current_dir(cwd)
        .status()
        .with_context(|| format!("run {} scripts/check-rsi-scorecard.py", python.display()))?;
    if !status.success() {
        bail!("scorecard exited with {status}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn serve_api(
    ledger: PathBuf,
    events: Option<PathBuf>,
    state: Option<PathBuf>,
    plan: Option<PathBuf>,
    controls: Option<PathBuf>,
    catalogue: Option<PathBuf>,
    graph: Option<PathBuf>,
    budget_usd: Option<String>,
    mission: Option<String>,
    host: &str,
    port: u16,
) -> Result<()> {
    let events = events.unwrap_or_else(|| ledger.with_file_name("events.jsonl"));
    let budget = budget_usd
        .as_deref()
        .map(|amount| {
            MicroUsd::parse_usd(amount)
                .map(Budget::new)
                .with_context(|| format!("--budget-usd {amount} is not a dollar amount"))
        })
        .transpose()?;
    let cockpit = Arc::new(Cockpit {
        ledger_path: ledger,
        events_path: events,
        state_path: state,
        plan_path: plan,
        controls_path: controls,
        catalogue_path: catalogue,
        graph_path: graph,
        budget,
        mission,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the harness API runtime")?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind((host, port))
            .await
            .with_context(|| format!("bind {host}:{port}"))?;
        let address = listener.local_addr().context("harness API address")?;
        println!("susi harness API listening on http://{address} (GET /api/status, GET /api/events, POST /api/command; JSON/SSE only, no page at /)");
        println!("POST /api/command requires Authorization: Bearer <api_auth_token> once the token is seeded; before that only loopback callers may write");
        serve(listener, cockpit, net_guard_authorizer())
            .await
            .context("serve the harness API")
    })
}

/// Core's zero-trust admission for harness API writes, the same policy GMCP,
/// GEMI and A2A apply: a valid member signature over the exact body, or the
/// seeded `api_auth_token` bearer; before seeding, loopback callers only.
fn net_guard_authorizer() -> CommandAuthorizer {
    Arc::new(|context: &CommandAuthContext<'_>| {
        admit_command(|name| context.header(name), context.peer, context.body)
    })
}

fn admit_command<'a>(
    header: impl Fn(&str) -> Option<&'a str>,
    peer: std::net::IpAddr,
    body: &[u8],
) -> bool {
    let signed = susi_core::net_guard::SignedRequest {
        node: header("x-susi-node"),
        ts_secs: header("x-susi-req-ts").and_then(|s| s.parse().ok()),
        nonce: header("x-susi-req-nonce"),
        sig: header("x-susi-req-sig"),
    };
    susi_core::net_guard::NetGuard::is_authorized(
        header("authorization"),
        peer,
        &signed,
        "POST",
        "/api/command",
        Some(body),
    )
}

#[allow(clippy::too_many_arguments)]
fn serve_gateway(
    port: u16,
    host: &str,
    catalogue_path: PathBuf,
    ledger_path: PathBuf,
    upstream: String,
    timeout: u64,
    accounts_path: Option<PathBuf>,
    cwd: &Path,
) -> Result<()> {
    // The serving rule comes first, before anything is created or bound:
    // this entry point bound any --host as given, so `--host 0.0.0.0` served
    // an open gateway that spent the operator's keys for anyone.
    let ingress = gateway_ingress(host, accounts_path, cwd)?;
    let resolved_cat = if catalogue_path.is_absolute() {
        catalogue_path
    } else {
        cwd.join(catalogue_path)
    };
    let resolved_ledger = if ledger_path.is_absolute() {
        ledger_path
    } else {
        cwd.join(ledger_path)
    };
    if let Some(parent) = resolved_ledger.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let catalogue = PriceCatalogue::load_or_builtin(&resolved_cat)
        .with_context(|| format!("load price catalogue {}", resolved_cat.display()))?;

    let cache_dir = resolved_ledger.parent().map(Path::to_path_buf);
    let state = Arc::new(susi_harness::gateway::GatewayState {
        catalogue,
        ledger_path: resolved_ledger,
        upstream_chat_url: upstream,
        provider: "susi".into(),
        api_key: None,
        timeout_secs: timeout,
        max_output_tokens: None,
        cache: Some(Arc::new(susi_harness::cache::ResponseCache::new(cache_dir))),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for harness gateway")?;

    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((host, port))
            .await
            .with_context(|| format!("bind harness gateway to {host}:{port}"))?;
        let addr = listener.local_addr()?;
        eprintln!("susi harness gateway listening on http://{addr}/v1 (full cache on)");

        if let Some(ingress) = ingress {
            susi_harness::ingress::serve(listener, state, ingress)
                .await
                .map_err(|e| crate::anyhow::anyhow!("{e}"))
        } else {
            susi_harness::gateway::serve(listener, state)
                .await
                .map_err(|e| crate::anyhow::anyhow!("{e}"))
        }
    })
}

/// The accounts the gateway authenticates callers against, read from
/// `accounts_path`, once the serving rule the harness binaries share allows
/// `host` with them: any address other machines can reach needs at least
/// one account.
fn gateway_ingress(
    host: &str,
    accounts_path: Option<PathBuf>,
    cwd: &Path,
) -> Result<Option<Arc<susi_harness::ingress::Ingress>>> {
    let ingress = match accounts_path {
        Some(accounts_file) => {
            let resolved = if accounts_file.is_absolute() {
                accounts_file
            } else {
                cwd.join(accounts_file)
            };
            let text = std::fs::read_to_string(&resolved)
                .with_context(|| format!("read accounts file {}", resolved.display()))?;
            let ingress = susi_harness::ingress::Ingress::parse(&text)
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            Some(Arc::new(ingress))
        }
        None => None,
    };
    susi_harness::ingress::require_accounts_off_loopback(host, ingress.as_deref())
        .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
    Ok(ingress)
}

#[cfg(test)]
mod tests {
    use super::HarnessCommands;
    use crate::cli::defs::{Cli, Commands};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn api_parses_the_graph_bus() {
        let cli = Cli::try_parse_from([
            "susi",
            "harness",
            "api",
            "--ledger",
            "out/ledger.jsonl",
            "--graph",
            "out/bus.jsonl",
        ])
        .unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Api { graph, .. } => {
                assert_eq!(graph, Some(PathBuf::from("out/bus.jsonl")));
            }
            _ => panic!("expected harness api"),
        }
    }

    #[test]
    fn a_remote_harness_command_without_a_token_is_refused() {
        // Plan A2: NetGuard refuses a tokenless write from a non-loopback
        // peer whether or not the host has seeded api_auth_token.
        let remote = std::net::IpAddr::from([203, 0, 113, 7]);
        assert!(!super::admit_command(|_| None, remote, b"{}"));
        assert!(!super::admit_command(
            |name| (name == "authorization").then_some("Bearer not-the-token"),
            remote,
            b"{}"
        ));
    }

    #[test]
    fn cockpit_stays_a_hidden_alias_of_api() {
        let cli =
            Cli::try_parse_from(["susi", "harness", "cockpit", "--ledger", "l.jsonl"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        assert!(matches!(action, HarnessCommands::Api { .. }));
        let mut command = <Cli as clap::CommandFactory>::command();
        let harness = command
            .find_subcommand_mut("harness")
            .expect("harness subcommand");
        let help = harness.render_long_help().to_string();
        assert!(help.contains("api"), "{help}");
        assert!(!help.contains("cockpit"), "{help}");
    }

    #[test]
    fn models_parses_without_a_gateway() {
        let cli = Cli::try_parse_from(["susi", "harness", "models"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models {
                verify,
                discover,
                provider,
                available,
            } => {
                assert!(!verify);
                assert!(!discover);
                assert!(!available);
                assert!(provider.is_none());
            }
            _ => panic!("expected harness models"),
        }
    }

    #[test]
    fn models_available_parses() {
        let cli = Cli::try_parse_from(["susi", "harness", "models", "--available"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models { available, .. } => {
                assert!(available);
            }
            _ => panic!("expected harness models --available"),
        }
    }

    #[test]
    fn gateway_parses_with_defaults() {
        let cli = Cli::try_parse_from(["susi", "harness", "gateway"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Gateway {
                port,
                host,
                upstream,
                ..
            } => {
                assert_eq!(port, 9180);
                assert_eq!(host, "127.0.0.1");
                assert_eq!(upstream, "direct");
            }
            _ => panic!("expected harness gateway"),
        }
    }

    /// `susi harness gateway --host 0.0.0.0` bound every interface and served
    /// an unauthenticated gateway on the operator's keys; `susi-harness`
    /// refused the same command. Both now share one rule.
    #[test]
    fn gateway_off_loopback_needs_an_accounts_file_with_an_account() {
        let dir = std::env::temp_dir().join(format!(
            "susi-gateway-accounts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("empty.json"), r#"{"accounts": []}"#).expect("write");
        std::fs::write(
            dir.join("one.json"),
            r#"{"accounts": [{"token": "alice-token", "name": "alice", "budget_usd": "1"}]}"#,
        )
        .expect("write");

        assert!(super::gateway_ingress("127.0.0.1", None, &dir)
            .expect("loopback serves open")
            .is_none());
        for host in ["0.0.0.0", "::", "192.168.1.10"] {
            let error = super::gateway_ingress(host, None, &dir)
                .expect_err("an open gateway off loopback")
                .to_string();
            assert!(error.contains("--accounts"), "{error}");
            let error = super::gateway_ingress(host, Some(PathBuf::from("empty.json")), &dir)
                .expect_err("an accounts file with no account")
                .to_string();
            assert!(error.contains("lists no account"), "{error}");
            let ingress = super::gateway_ingress(host, Some(PathBuf::from("one.json")), &dir)
                .expect("an authenticated gateway")
                .expect("an ingress");
            assert!(!ingress.is_open());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn models_verify_parses_with_a_provider() {
        let cli = Cli::try_parse_from([
            "susi",
            "harness",
            "models",
            "--verify",
            "--provider",
            "gemini",
        ])
        .unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models {
                verify, provider, ..
            } => {
                assert!(verify);
                assert_eq!(provider.as_deref(), Some("gemini"));
            }
            _ => panic!("expected harness models --verify"),
        }
    }

    #[test]
    fn models_provider_requires_verify_or_discover() {
        assert!(
            Cli::try_parse_from(["susi", "harness", "models", "--provider", "gemini"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "susi",
            "harness",
            "models",
            "--verify",
            "--discover",
            "--provider",
            "gemini"
        ])
        .is_err());
        let cli = Cli::try_parse_from([
            "susi",
            "harness",
            "models",
            "--discover",
            "--provider",
            "deepinfra",
        ])
        .unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models {
                discover,
                verify,
                provider,
                ..
            } => {
                assert!(discover && !verify);
                assert_eq!(provider.as_deref(), Some("deepinfra"));
            }
            _ => panic!("expected harness models --discover"),
        }
    }
    #[test]
    fn daily_cap_sums_only_todays_priced_entries() {
        let dir = std::env::temp_dir().join(format!("susi-daily-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let ledger = dir.join("ledger.jsonl");
        let now = 1_792_950_000_000u64;
        let yesterday = now - 86_400_000;
        let body = format!(
            "{{\"started_unix_ms\":{now},\"cost\":3000000}}\n{{\"started_unix_ms\":{yesterday},\"cost\":10000000}}\n{{\"started_unix_ms\":{now},\"cost\":0,\"cached\":true}}\n"
        );
        std::fs::write(&ledger, body).expect("ledger");
        assert_eq!(
            susi_harness::operator::spent_today(&ledger, now, &mut |_: String| {}).micros(),
            3_000_000
        );
        assert_eq!(
            susi_harness::operator::spent_today(
                &dir.join("missing.jsonl"),
                now,
                &mut |_: String| {}
            )
            .micros(),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crash cut a rejected row inside a multi-byte character of its
    /// reason, with no newline after it, and the next row was appended onto
    /// the torn line. Read as one string, the file was not UTF-8 and the day
    /// read as nothing spent, so `--daily-usd` let every later run start.
    #[test]
    fn daily_cap_counts_every_row_around_a_tear_inside_a_character() {
        let dir = std::env::temp_dir().join(format!(
            "susi-daily-cap-torn-{}-{}",
            std::process::id(),
            susi_harness::operator::now_unix_ms()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let ledger = dir.join("ledger.jsonl");
        let now = 1_792_950_000_000u64;
        let rejected = format!(
            "{{\"started_unix_ms\":{now},\"cost\":0,\"outcome\":{{\"rejected\":{{\"reason\":\"gate “red”\"}}}}}}"
        );
        let cut = rejected.find('“').expect("a multi-byte character") + 1;
        let mut bytes = format!("{{\"started_unix_ms\":{now},\"cost\":3000000}}\n").into_bytes();
        bytes.extend_from_slice(&rejected.as_bytes()[..cut]);
        bytes.extend_from_slice(
            format!("{{\"started_unix_ms\":{now},\"cost\":2000000}}\n").as_bytes(),
        );
        std::fs::write(&ledger, &bytes).expect("ledger");
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "the tear must cut a character"
        );
        assert_eq!(
            susi_harness::operator::spent_today(&ledger, now, &mut |_: String| {}).micros(),
            5_000_000
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ASCII tear: the next row lands on the fragment's line. The daily
    /// cap dropped that real charge with the fragment.
    #[test]
    fn daily_cap_counts_a_row_glued_onto_an_ascii_tear() {
        let dir = std::env::temp_dir().join(format!(
            "susi-daily-cap-glued-{}-{}",
            std::process::id(),
            susi_harness::operator::now_unix_ms()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let ledger = dir.join("ledger.jsonl");
        let now = 1_792_950_000_000u64;
        let body = format!(
            "{{\"started_unix_ms\":{now},\"cost\":1000000}}\n{{\"started_unix_ms\":{now},\"co{{\"started_unix_ms\":{now},\"cost\":4000000}}\n"
        );
        std::fs::write(&ledger, body).expect("ledger");
        assert_eq!(
            susi_harness::operator::spent_today(&ledger, now, &mut |_: String| {}).micros(),
            5_000_000
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The three `--ledger` help texts put the workspace ahead of
    /// `SUSI_HARNESS_LEDGER`; the code reads the setting first.
    #[test]
    fn ledger_help_gives_the_order_the_default_is_resolved_in() {
        use clap::CommandFactory;
        let cli = Cli::command();
        let harness = cli.find_subcommand("harness").expect("the harness command");
        for name in ["run", "budget", "gateway"] {
            let help = harness
                .find_subcommand(name)
                .and_then(|command| {
                    command
                        .get_arguments()
                        .find(|argument| argument.get_id() == "ledger")
                })
                .and_then(|argument| argument.get_help())
                .map(ToString::to_string)
                .unwrap_or_else(|| panic!("harness {name} --ledger has help"));
            let setting = help.find("SUSI_HARNESS_LEDGER").expect("names the setting");
            let workspace = help.find("out/ledger.jsonl").expect("names the workspace");
            let home = help.find("data home").expect("names the data home");
            assert!(setting < workspace && workspace < home, "{name}: {help}");
        }
        let home = std::env::temp_dir();
        let checkout = std::env::temp_dir().join(format!(
            "susi-ledger-order-{}-{}",
            std::process::id(),
            susi_harness::operator::now_unix_ms()
        ));
        std::fs::create_dir_all(checkout.join("deploy/harness")).expect("checkout");
        std::fs::write(checkout.join("deploy/harness/prices.json"), "{}").expect("prices");
        assert_eq!(
            susi_harness::fleet::ledger_for(&checkout, Some("x.jsonl".into()), &home),
            PathBuf::from("x.jsonl"),
            "the setting wins inside a workspace, as the help says"
        );
        let _ = std::fs::remove_dir_all(&checkout);
    }

    /// A model refused before the run started was in no task's ladder, so the
    /// before-each-task probe never reached it and it stayed unused all
    /// mission after a top-up.
    #[test]
    fn a_mission_keeps_the_models_refused_at_its_start_within_reach_of_a_probe() {
        let fleet = susi_harness::fleet::Fleet {
            ladder: vec!["deepseek-flash".into(), "deepseek-pro".into()],
            standby: vec!["gemini-flash".into(), "deepseek-pro".into()],
            ..susi_harness::fleet::Fleet::default()
        };
        assert_eq!(
            susi_harness::operator::mission_alternates(&fleet),
            vec!["deepseek-flash", "deepseek-pro", "gemini-flash"]
        );
    }
}
