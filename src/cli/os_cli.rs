//! `susi os` — the operating-system view of the substrate in one shot:
//! consensus state (term / leader / decisions from the replicated
//! ledger), the in-process swarm host (orchestrator / workers /
//! identities), the leaf-service process table, and the verified peer
//! roster. This is the operator's answer to "is the OS for agents
//! healthy right now?" — ledger/peers are persisted kernel state (accurate
//! even when the daemon is down); swarm host is the in-process composition.

use crate::anyhow::Result;
use std::path::Path;

pub use susi_cli_defs::os::*;

pub fn execute(action: Option<OsCommands>, top_json: bool, workspace: &Path) -> Result<()> {
    match action.unwrap_or(OsCommands::Status { json: false }) {
        // `susi_daemon::os_status` builds the view; the MCP `os_status`
        // tool returns the same JSON.
        OsCommands::Status { json } => {
            if json || top_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&susi_daemon::os_status::report())?
                );
            } else {
                print!("{}", susi_daemon::os_status::render());
            }
            Ok(())
        }
        OsCommands::Clean => clean(),
        OsCommands::Ecosystem { json, doctor } => ecosystem(json || top_json, doctor, workspace),
        OsCommands::Runtime { json } => super::os_runtime::print(json || top_json),
        OsCommands::Route {
            json,
            requires,
            max_cost,
            no_cloud,
        } => route(json || top_json, requires.as_deref(), max_cost, !no_cloud),
        OsCommands::RouteReset { provider } => route_reset(&provider, top_json, workspace),
        OsCommands::Manage { resource } => manage(resource, workspace),
        OsCommands::Provision { action } => provision(action, top_json),
        OsCommands::Capabilities { action } => capabilities(action, top_json, workspace),
    }
}

fn capabilities(
    action: Option<Box<OsCapabilitiesAction>>,
    top_json: bool,
    workspace: &Path,
) -> Result<()> {
    use susi_core::capability_bus::CapabilityRequest;
    use susi_core::registry::CapabilityRegistry;

    let registry = CapabilityRegistry::global();
    match action.map_or(OsCapabilitiesAction::Discover { json: false }, |a| *a) {
        OsCapabilitiesAction::Discover { json } => {
            let discovered = registry.typed_bus().discover();
            if json || top_json {
                println!("{}", serde_json::to_string_pretty(&discovered)?);
            } else {
                println!("SUSI OS — capability bus ({} discovered)", discovered.len());
                for d in &discovered {
                    println!(
                        "  {id:<28} {surface:?} permission={permission} timeout_ms={timeout_ms} cost_micros={cost_micros}",
                        id = d.id,
                        surface = d.surface,
                        permission = d.permission,
                        timeout_ms = d.timeout_ms,
                        cost_micros = d.cost_micros,
                    );
                }
            }
            Ok(())
        }
        OsCapabilitiesAction::Invoke {
            id,
            subject,
            permission,
            args,
            timeout_ms,
            budget_micros,
            delegation,
            json,
        } => {
            let args: serde_json::Value = serde_json::from_str(&args)
                .map_err(|e| crate::anyhow::anyhow!("--args is not valid JSON: {e}"))?;
            let request =
                CapabilityRequest::new(id, subject, [permission], timeout_ms, budget_micros);
            let delegation = parse_delegation(delegation)?;
            let receipt = registry
                .dispatch_nested(delegation.as_ref(), &request, &args, workspace)
                .map_err(|e| crate::anyhow::anyhow!("{e:?}"))?;
            if json || top_json {
                println!("{}", serde_json::to_string_pretty(&receipt)?);
            } else {
                println!(
                    "dispatched {} on {:?}: {}ms, {} micros",
                    receipt.capability_id, receipt.surface, receipt.elapsed_ms, receipt.cost_micros
                );
                println!("{}", receipt.output);
            }
            Ok(())
        }
        OsCapabilitiesAction::Delegate {
            id,
            subject,
            permission,
            timeout_ms,
            budget_micros,
            delegation,
        } => {
            let request =
                CapabilityRequest::new(id, subject, [permission], timeout_ms, budget_micros);
            let parent = parse_delegation(delegation)?;
            let envelope = registry
                .issue_delegation(parent.as_ref(), &request)
                .map_err(|e| crate::anyhow::anyhow!("{e:?}"))?;
            // The envelope is the output either way: it is what the driver
            // hands to the component that will run under it.
            println!("{}", serde_json::to_string(&envelope)?);
            Ok(())
        }
        OsCapabilitiesAction::Revoke { id, json } => {
            let id = id.trim();
            if id.is_empty() {
                crate::anyhow::bail!("envelope id must not be empty");
            }
            let local_revoked = registry
                .revoke_delegation(id)
                .map_err(|e| crate::anyhow::anyhow!("{e:?}"))?;
            let bus_root = susi_paths::SusiDirs::cache_dir().join("bus");
            let self_pid = std::process::id().to_string();
            let sibling = revoke_on_sibling_bus(&bus_root, &self_pid, id);
            let daemon_revoked = (sibling.contacted > 0).then_some(sibling.revoked);
            let revoked = local_revoked || sibling.revoked;
            if json || top_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "envelope_id": id,
                        "revoked": revoked,
                        "local_revoked": local_revoked,
                        "daemon_contacted": sibling.contacted,
                        "daemon_revoked": daemon_revoked,
                        "cancelled_runs": sibling.cancelled_runs,
                    }))?
                );
            } else if revoked && !sibling.cancelled_runs.is_empty() {
                println!(
                    "Revoked delegation envelope `{id}` (cancelled {} daemon run(s): {}).",
                    sibling.cancelled_runs.len(),
                    sibling.cancelled_runs.join(", ")
                );
            } else if revoked {
                println!("Revoked delegation envelope `{id}`.");
            } else if sibling.contacted > 0 {
                println!(
                    "Delegation envelope `{id}` was not live in this process or daemon ledger."
                );
            } else {
                println!("Delegation envelope `{id}` was not live in this process ledger.");
            }
            Ok(())
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SiblingRevokeReport {
    contacted: usize,
    revoked: bool,
    cancelled_runs: Vec<String>,
}

/// Forward `daemon.harness` `revoke_delegation` to every live sibling Susi
/// process (`pid != self_pid`) registered under `bus_root` (`<cache>/bus`), so
/// revoking an envelope from the CLI cancels daemon-hosted missions and trees
/// opened under `id` without hitting the CLI's own in-process prefix handler.
/// When no sibling bus directory or secret exists, returns immediately with
/// zero network I/O.
fn revoke_on_sibling_bus(bus_root: &Path, self_pid: &str, id: &str) -> SiblingRevokeReport {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let mut report = SiblingRevokeReport::default();
    let Some(secret) = std::fs::read_to_string(bus_root.join("secret"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() == 64)
    else {
        return report;
    };
    let Ok(entries) = std::fs::read_dir(bus_root) else {
        return report;
    };
    let Ok(body) = serde_json::to_string(&serde_json::json!({
        "topic": susi_core::plane_bus::topics::DAEMON_HARNESS,
        "payload": {
            "action": "revoke_delegation",
            "id": id,
        },
    })) else {
        return report;
    };

    let mut pid_dirs: Vec<std::path::PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let text = name.to_str()?;
            if text.is_empty()
                || text == self_pid
                || !text.bytes().all(|b| b.is_ascii_digit())
                || !entry.file_type().is_ok_and(|ft| ft.is_dir())
            {
                return None;
            }
            Some(entry.path())
        })
        .collect();
    pid_dirs.sort();

    for pid_dir in pid_dirs {
        // The sibling's routes live in its `routes*.json` manifest (or the
        // legacy `topics/` / `prefixes/` files of an older copy).
        let candidate = susi_core::plane_bus_ipc::route_candidates(
            std::slice::from_ref(&pid_dir),
            susi_core::plane_bus::topics::DAEMON_HARNESS,
        )
        .into_iter()
        .map(|(_, addr)| addr)
        .find(|addr| addr.ip().is_loopback());
        let Some(addr) = candidate else { continue };

        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(200)) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let req = format!(
            "POST /handle HTTP/1.0\r\nHost: 127.0.0.1\r\nX-Susi-Bus-Key: {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        if stream.write_all(req.as_bytes()).is_err() {
            continue;
        }
        let mut resp = String::new();
        if stream.read_to_string(&mut resp).is_err() {
            continue;
        }
        let Some(json_body) = resp.split("\r\n\r\n").nth(1) else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<serde_json::Value>(json_body) else {
            continue;
        };
        if envelope.get("miss").is_some() {
            continue;
        }
        if let Some(ok) = envelope.get("ok") {
            let result = ok.get("result").unwrap_or(ok);
            report.contacted = report.contacted.saturating_add(1);
            if result.get("revoked").and_then(serde_json::Value::as_bool) == Some(true) {
                report.revoked = true;
            }
            for run in result
                .get("cancelled_runs")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
            {
                if !report.cancelled_runs.iter().any(|r| r == run) {
                    report.cancelled_runs.push(run.to_string());
                }
            }
        }
    }
    report
}

/// `--delegation` text as JSON; the registry verifies the envelope itself.
fn parse_delegation(text: Option<String>) -> Result<Option<serde_json::Value>> {
    text.map(|text| serde_json::from_str::<serde_json::Value>(&text))
        .transpose()
        .map_err(|e| crate::anyhow::anyhow!("--delegation is not valid JSON: {e}"))
}

fn provision(action: OsProvisionAction, top_json: bool) -> Result<()> {
    // `susi_daemon::os_provision` builds every document; the MCP
    // `os_provision` tool returns the same ones.
    match action {
        OsProvisionAction::Probe { json } => {
            let report = susi_daemon::os_provision::probe_report();
            if json || top_json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("provision probe (tick contract: version-only, no list/apply)");
                for c in report["inventory"].as_array().into_iter().flatten() {
                    let flag = if c["available"].as_bool() == Some(true) {
                        "ok"
                    } else {
                        "missing"
                    };
                    println!(
                        "  {kind} ({tool}): {flag} — {summary}",
                        kind = c["kind"].as_str().unwrap_or_default(),
                        tool = c["tool"].as_str().unwrap_or_default(),
                        summary = c["summary"].as_str().unwrap_or_default()
                    );
                }
            }
        }
        OsProvisionAction::List { kind, json } => {
            let report =
                susi_daemon::os_provision::list_report(&kind).map_err(crate::anyhow::Error::msg)?;
            if json || top_json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report["nodes"].as_str().unwrap_or_default());
            }
        }
        OsProvisionAction::Apply { kind, manifest } => {
            let text = std::fs::read_to_string(&manifest)?;
            let out = susi_daemon::os_provision::apply(&kind, &text)
                .map_err(crate::anyhow::Error::msg)?;
            println!("{out}");
        }
    }
    Ok(())
}

fn route_reset(provider: &str, json: bool, workspace: &Path) -> Result<()> {
    let provider = provider.trim();
    if !susi_gemi::susi_core::plane_bus::gemi::valid_provider_id(provider) {
        crate::anyhow::bail!("provider must be 1-256 printable non-whitespace ASCII characters");
    }
    let cleared =
        susi_gemi::susi_core::plane_bus::gemi::ModelManager::clear_provider_cooldown(provider)
            .map_err(crate::anyhow::Error::msg)?;
    if cleared {
        susi_gemi::susi_sandbox::manager::SusiAuditLogger::log_event(
            workspace,
            "INFERENCE_PROVIDER_READMITTED",
            provider,
        );
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "provider": provider,
                "cleared": cleared,
            }))?
        );
    } else if cleared {
        println!("Re-admitted provider `{provider}` to inference placement.");
    } else {
        println!("Provider `{provider}` was not quarantined.");
    }
    Ok(())
}

fn route(
    json: bool,
    requires: Option<&str>,
    max_cost: Option<f64>,
    allow_cloud: bool,
) -> Result<()> {
    susi_gemi::engines::http_provider::register_configured_cloud_endpoints(
        susi_gemi::susi_core::registry::CapabilityRegistry::global(),
    );
    let providers = susi_gemi::susi_core::registry::CapabilityRegistry::global().list_providers();
    let decision = susi_gemi::engines::routing::InferenceRouter::plan_placement_for(
        &providers,
        requires,
        max_cost,
        allow_cloud,
    );
    if json {
        println!("{}", serde_json::to_string_pretty(&decision)?);
        return Ok(());
    }
    println!("SUSI OS — inference placement");
    println!("target:    {}", decision.target);
    println!("policy:    {}", decision.policy);
    println!(
        "provider:  {}",
        decision.provider.as_deref().unwrap_or("local runtime")
    );
    println!(
        "model:     {}",
        decision.local_model.as_deref().unwrap_or("auto-select")
    );
    println!("ready:     {}", decision.local_ready);
    println!("cloud:     {}", decision.allow_cloud);
    if let Some(requires) = decision.requires.as_deref() {
        println!("requires:  {requires}");
    }
    if let Some(max_cost) = decision.max_cost {
        println!("max cost:  ${max_cost:.4}");
    }
    println!("reason:    {}", decision.reason);
    println!("clouds:    {}", decision.cloud_candidates.len());
    println!("cooled:    {}", decision.cooled_candidates.len());
    Ok(())
}

fn manage(resource: OsManageCommands, workspace: &Path) -> Result<()> {
    use super::{agent_cli, framework_cli, mcp_cli, model_cli, services_cli};
    match resource {
        OsManageCommands::Service { action } => {
            let action = match action {
                OsServiceAction::Status => services_cli::ServicesCommands::Status { json: false },
                OsServiceAction::Start { name } => services_cli::ServicesCommands::Start { name },
                OsServiceAction::Stop { name } => services_cli::ServicesCommands::Stop { name },
                OsServiceAction::Restart { name } => {
                    services_cli::ServicesCommands::Restart { name }
                }
                OsServiceAction::Logs { name, lines } => {
                    services_cli::ServicesCommands::Logs { name, lines }
                }
            };
            services_cli::execute(Some(action), workspace)
        }
        OsManageCommands::Model { action } => {
            let action = match action {
                OsModelAction::List => model_cli::ModelCommands::List,
                OsModelAction::Doctor { name } => model_cli::ModelCommands::Doctor { model: name },
                OsModelAction::Install { source }
                    if source.trim().eq_ignore_ascii_case(
                        susi_gemi::models::zc_first_model::STARTER_SOURCE,
                    ) =>
                {
                    // The user's explicit request: the only path that
                    // downloads the starter (core never does on its own).
                    let starter = susi_gemi::models::NoLocalModelInstalled::starter();
                    println!(
                        "Installing {} and its tokenizer (a download of about {} MB unless already installed)...",
                        starter.model,
                        starter.size_mb()
                    );
                    let outcome = with_download_progress(|| {
                        susi_gemi::models::ModelManager::install_starter_model_on_request(workspace)
                    })?;
                    if let Some(reason) = outcome.terminal_reason() {
                        crate::anyhow::bail!("{reason}");
                    }
                    println!("{}", outcome.summary());
                    return Ok(());
                }
                OsModelAction::Install { source }
                    if source
                        .trim()
                        .eq_ignore_ascii_case(susi_gmcp::embed_provider::INSTALL_SOURCE) =>
                {
                    // The user's explicit request: the only path that
                    // downloads the semantic-search embedding model.
                    println!(
                        "Installing the BAAI/bge-small-en-v1.5 embedding model (a download unless already installed)..."
                    );
                    let installed = susi_gmcp::embed_provider::install_on_request()?;
                    println!("{installed}");
                    return Ok(());
                }
                OsModelAction::Install { source } => {
                    let installed =
                        susi_gemi::models::ModelManager::install_model_foreground(&source)
                            .map_err(crate::anyhow::Error::msg)?;
                    println!("{installed}");
                    return Ok(());
                }
                OsModelAction::Uninstall { name } => {
                    uninstall_managed_model(&name)?;
                    return Ok(());
                }
                OsModelAction::Load { name } => {
                    // Large models can take minutes to read and place; the
                    // daemon's MCP sessions are kept open at least this long.
                    let result = super::os_runtime::daemon_tool(
                        "runtime_models_load",
                        &serde_json::json!({ "model": name }),
                        susi_gmcp::server::MAX_TOOL_CALL_SECS,
                    )
                    .map_err(crate::anyhow::Error::msg)?;
                    println!(
                        "loaded {}",
                        result["loaded"].as_str().unwrap_or(name.as_str())
                    );
                    return Ok(());
                }
                OsModelAction::Unload { name } => {
                    let result = super::os_runtime::daemon_tool(
                        "runtime_models_unload",
                        &serde_json::json!({ "model": name }),
                        30,
                    )
                    .map_err(crate::anyhow::Error::msg)?;
                    let path = result["path"].as_str().unwrap_or(name.as_str());
                    match (result["unloaded"].as_bool(), result["in_flight_users"].as_u64()) {
                        (Some(true), Some(0) | None) => println!("unloaded {path}"),
                        (Some(true), Some(users)) => println!(
                            "unloaded {path}; memory is released when {users} in-flight request(s) finish"
                        ),
                        (Some(false) | None, _) => println!("{path} was not loaded"),
                    }
                    return Ok(());
                }
                OsModelAction::Prefer { name } => model_cli::ModelCommands::Prefer { model: name },
                OsModelAction::Configure { name, file } => {
                    model_cli::ModelCommands::Configure { model: name, file }
                }
                OsModelAction::Reset { name } => model_cli::ModelCommands::Reset { model: name },
            };
            model_cli::execute(Some(action), workspace)
        }
        OsManageCommands::Mcp { action } => {
            let action = match action {
                OsMcpAction::List => mcp_cli::McpCommands::List,
                OsMcpAction::Doctor { name } => mcp_cli::McpCommands::Doctor { server: name },
                OsMcpAction::Install { name } => mcp_cli::McpCommands::Enable { server: name },
                OsMcpAction::Uninstall { name } => mcp_cli::McpCommands::Disable { server: name },
                OsMcpAction::Configure { name, file } => {
                    mcp_cli::McpCommands::Configure { server: name, file }
                }
                OsMcpAction::Reset { name } => mcp_cli::McpCommands::Reset { server: name },
                OsMcpAction::Status => mcp_cli::McpCommands::Status,
            };
            mcp_cli::execute(Some(action), workspace).map(|_| ())
        }
        OsManageCommands::Agent { action } => {
            let action = match action {
                OsAgentAction::List => agent_cli::AgentCommands::List,
                OsAgentAction::Doctor { name } => agent_cli::AgentCommands::Doctor { agent: name },
                OsAgentAction::Configure { name, file } => {
                    agent_cli::AgentCommands::Configure { agent: name, file }
                }
                OsAgentAction::Reset { name } => agent_cli::AgentCommands::Reset { agent: name },
                OsAgentAction::Run { name, wait, prompt } => agent_cli::AgentCommands::Run {
                    agent: name,
                    wait,
                    prompt,
                },
                OsAgentAction::Tasks => agent_cli::AgentCommands::Tasks,
                OsAgentAction::Cancel { task_id } => agent_cli::AgentCommands::Cancel { task_id },
            };
            agent_cli::execute(action, workspace)
        }
        OsManageCommands::Framework { action } => {
            let action = match action {
                OsFrameworkAction::List => framework_cli::FrameworkCommands::List,
                OsFrameworkAction::Doctor { name } => {
                    framework_cli::FrameworkCommands::Doctor { engine: name }
                }
                OsFrameworkAction::Configure { name, file } => {
                    framework_cli::FrameworkCommands::Configure { engine: name, file }
                }
                OsFrameworkAction::Reset { name } => {
                    framework_cli::FrameworkCommands::Reset { engine: name }
                }
                OsFrameworkAction::Run { name, wait, prompt } => {
                    framework_cli::FrameworkCommands::Run {
                        engine: name,
                        wait,
                        prompt,
                    }
                }
                OsFrameworkAction::Tasks => framework_cli::FrameworkCommands::Tasks,
                OsFrameworkAction::Cancel { task_id } => {
                    framework_cli::FrameworkCommands::Cancel { task_id }
                }
            };
            framework_cli::execute(action, workspace)
        }
    }
}

/// Run an explicit foreground model download and print its progress every
/// ~5 s, so a long download never looks frozen. Progress comes from the
/// record the transfer loop writes (`download_progress.json` in the data
/// dir); a record older than this command is ignored.
fn with_download_progress<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    let record = susi_paths::SusiDirs::data_dir().join("download_progress.json");
    let started = std::time::SystemTime::now();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let out = work();
            let _ = done_tx.send(());
            out
        });
        let mut last_line = String::new();
        while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            done_rx.recv_timeout(std::time::Duration::from_secs(5))
        {
            if let Some(line) = download_progress_line(&record, started) {
                if line != last_line {
                    println!("{line}");
                    last_line = line;
                }
            }
        }
        match worker.join() {
            Ok(out) => out,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// `Downloading <model>: 37% (159 of 429 MB)` from a progress record written
/// after `since`, or `None` when there is no fresh running download.
fn download_progress_line(record: &Path, since: std::time::SystemTime) -> Option<String> {
    let fresh = std::fs::metadata(record)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| modified >= since);
    if !fresh {
        return None;
    }
    let progress: serde_json::Value = serde_json::from_slice(&std::fs::read(record).ok()?).ok()?;
    if progress.get("status").and_then(serde_json::Value::as_str) != Some("RUNNING") {
        return None;
    }
    let done = progress.get("bytes_downloaded")?.as_u64()?;
    let total = progress.get("expected_bytes")?.as_u64()?;
    let model = progress
        .get("model_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("model");
    let mb = |bytes: u64| bytes.saturating_add(500_000) / 1_000_000;
    Some(match done.saturating_mul(100).checked_div(total) {
        Some(percent) => format!(
            "Downloading {model}: {percent}% ({} of {} MB)",
            mb(done),
            mb(total)
        ),
        None => format!("Downloading {model}: {} MB so far", mb(done)),
    })
}

// `susi_daemon::host_views::model_uninstall` removes the model; the MCP
// `model_uninstall` tool calls the same function (reading the daemon's
// inference cache in process instead of over the daemon's MCP socket).
fn uninstall_managed_model(name: &str) -> Result<()> {
    let daemon_running =
        susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir()).is_some();
    let loaded: &dyn Fn() -> Result<serde_json::Value, String> =
        &super::os_runtime::daemon_loaded_models;
    let doc = susi_daemon::host_views::model_uninstall(name, daemon_running.then_some(loaded))
        .map_err(crate::anyhow::Error::msg)?;
    println!("{}", doc["message"].as_str().unwrap_or_default());
    Ok(())
}

// `susi_daemon::ecosystem_view` builds the document; the MCP `os_ecosystem`
// tool returns the same one.
fn ecosystem(json: bool, doctor: bool, workspace: &Path) -> Result<()> {
    let body =
        susi_daemon::ecosystem_view::report(workspace, super::os_runtime::daemon_loaded_models)
            .map_err(crate::anyhow::Error::msg)?;
    let summary = &body["summary"];
    let unhealthy_installed = summary["unhealthy_installed_runtimes"]
        .as_u64()
        .unwrap_or(0);
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        let hardware = &body["hardware"];
        let runtimes = body["runtimes"].as_array().cloned().unwrap_or_default();
        println!("SUSI OS — local AI ecosystem");
        println!(
            "hardware:   {} CPU(s), {}GB RAM, {}",
            hardware["cpus"],
            hardware["ram_gb"],
            hardware["gpu_info"].as_str().unwrap_or_default()
        );
        println!(
            "runtimes:   {}/{} installed",
            summary["installed_runtimes"],
            runtimes.len()
        );
        for runtime in &runtimes {
            println!(
                "  {:<14} {:<9} {}",
                runtime["id"].as_str().unwrap_or_default(),
                if runtime["installed"].as_bool() == Some(true) {
                    "installed"
                } else {
                    "missing"
                },
                runtime["version"].as_str().unwrap_or_default()
            );
        }
        println!("models:     {} discovered", summary["models"]);
        for model in body["models"].as_array().into_iter().flatten() {
            println!(
                "  {} — {}",
                model
                    .get("id")
                    .or_else(|| model.get("model_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
                model
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("managed")
            );
        }
        println!("agents:     {} catalogued", summary["agents"]);
        println!("frameworks: {} catalogued", summary["frameworks"]);
        println!(
            "MCP:        {} catalogued/configured",
            summary["mcp_servers"]
        );
        println!(
            "processes:  {} AI-related process(es) running",
            summary["running_ai_processes"]
        );
        println!();
        println!("Manage: `susi os manage <service|model|mcp|agent|framework> …`");
    }
    if doctor && unhealthy_installed > 0 {
        crate::anyhow::bail!(
            "{unhealthy_installed} installed AI runtime(s) failed their version probe"
        );
    }
    Ok(())
}

/// Remove the same files `stray_bin_warnings` flags — stray binaries in
/// `~/.susi/bin` at or above the warn threshold. Only files the warning
/// predicate already names are touched; the live binary and `lib/` are
/// never removed. `susi_core::substrate_hygiene::clean` is the same pass
/// the MCP `clean` tool runs.
pub(crate) fn clean() -> Result<()> {
    let report = susi_core::substrate_hygiene::clean();
    for item in &report.removed {
        println!(
            "removed {} ({})",
            item.path.display(),
            susi_core::substrate_hygiene::human_bytes(item.bytes)
        );
    }
    for (path, error) in &report.failed {
        eprintln!("could not remove {}: {error}", path.display());
    }
    println!(
        "cleaned {} file(s), reclaimed {}",
        report.removed_count, report.reclaimed
    );
    Ok(())
}

#[cfg(test)]
mod download_progress_tests {
    use super::download_progress_line;

    #[test]
    fn a_fresh_running_record_prints_percent_and_megabytes() {
        let dir = std::env::temp_dir().join(format!("susi-dl-progress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("download_progress.json");
        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(2);
        std::fs::write(
            &record,
            r#"{"model_name":"qwen.gguf","bytes_downloaded":159000000,"expected_bytes":429000000,"status":"RUNNING"}"#,
        )
        .unwrap();
        assert_eq!(
            download_progress_line(&record, before).as_deref(),
            Some("Downloading qwen.gguf: 37% (159 of 429 MB)")
        );
        // A record from before this command started is not this download.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        assert_eq!(download_progress_line(&record, later), None);
        std::fs::write(
            &record,
            r#"{"model_name":"qwen.gguf","bytes_downloaded":1,"expected_bytes":1,"status":"COMPLETED"}"#,
        )
        .unwrap();
        assert_eq!(download_progress_line(&record, before), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_malformed_record_returns_none() {
        let dir =
            std::env::temp_dir().join(format!("susi-dl-progress-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("download_progress.json");
        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(2);
        assert_eq!(download_progress_line(&record, before), None);

        std::fs::write(&record, b"{not-valid-json").unwrap();
        assert_eq!(download_progress_line(&record, before), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_expected_bytes_reports_megabytes_so_far_without_dividing_by_zero() {
        let dir =
            std::env::temp_dir().join(format!("susi-dl-progress-zero-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("download_progress.json");
        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(2);
        std::fs::write(
            &record,
            r#"{"model_name":"stream.gguf","bytes_downloaded":5000000,"expected_bytes":0,"status":"RUNNING"}"#,
        )
        .unwrap();
        assert_eq!(
            download_progress_line(&record, before).as_deref(),
            Some("Downloading stream.gguf: 5 MB so far")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_download_progress_returns_closure_output_immediately() {
        let out = super::with_download_progress(|| 42usize);
        assert_eq!(out, 42);
    }
}

#[cfg(test)]
mod capabilities_revoke_tests {
    use super::{capabilities, OsCapabilitiesAction};
    use std::path::Path;
    use susi_core::capability_bus::{CapabilityError, CapabilityRequest};
    use susi_core::registry::{CapabilityRegistry, Tool};

    struct RevokeProbeTool;
    impl Tool for RevokeProbeTool {
        fn name(&self) -> &str {
            "cli_revoke_probe"
        }
        fn description(&self) -> &str {
            "probe for cli delegation revocation"
        }
        fn execute(
            &self,
            args: &serde_json::Value,
            _workspace: &Path,
        ) -> susi_core::susi_error::EaiResult<String> {
            Ok(args.to_string())
        }
    }

    #[test]
    fn capabilities_revoke_rejects_blank_id_and_cancels_issued_envelope() {
        let ws = std::env::temp_dir();
        let err = capabilities(
            Some(Box::new(OsCapabilitiesAction::Revoke {
                id: "   ".to_string(),
                json: true,
            })),
            false,
            &ws,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));

        // Ensure host MAC key exists in current SUSI_HOME so host DelegationAuthority is active.
        let _ =
            susi_core::mac_policy::MacPolicy::global().derive_key(b"susi-delegation-envelope/v1");
        let registry = CapabilityRegistry::global();
        registry.register_tool(RevokeProbeTool);

        let request = CapabilityRequest::new(
            "cli_revoke_probe",
            "worker-cli-revoke",
            ["capability:tool".to_string()],
            30_000,
            1_000,
        );
        let envelope = registry
            .issue_delegation(None, &request)
            .expect("host authority issues signed envelope");
        let envelope_val = serde_json::to_value(&envelope).expect("envelope serializes");
        let envelope_id = envelope_val["id"]
            .as_str()
            .expect("signed envelope carries id")
            .to_string();

        capabilities(
            Some(Box::new(OsCapabilitiesAction::Revoke {
                id: envelope_id.clone(),
                json: true,
            })),
            false,
            &ws,
        )
        .expect("revoke command succeeds");

        // Second revoke on an already-cancelled id is idempotent (`revoked: false`, still Ok).
        capabilities(
            Some(Box::new(OsCapabilitiesAction::Revoke {
                id: envelope_id,
                json: false,
            })),
            false,
            &ws,
        )
        .expect("idempotent revoke command succeeds");

        let child_req = CapabilityRequest::new(
            "cli_revoke_probe",
            "nested-worker",
            ["capability:tool".to_string()],
            10_000,
            100,
        );
        let refused =
            registry.dispatch_nested(Some(&envelope_val), &child_req, &serde_json::json!({}), &ws);
        assert!(
            matches!(refused, Err(CapabilityError::Delegation(_))),
            "expected revoked envelope to fail Delegation check, got {refused:?}"
        );
    }

    #[test]
    fn revoke_on_sibling_bus_cancels_daemon_envelope_and_stays_noop_offline() {
        use std::sync::Arc;
        use susi_core::delegation::{
            DelegationAuthority, DelegationHop, DelegationLimits, DelegationScope,
        };
        use susi_core::plane_bus::{topics, PlaneHandler};
        use susi_core::plane_bus_ipc::IpcPlaneBus;

        struct SiblingHarnessHandler {
            authority: Arc<DelegationAuthority>,
        }
        impl PlaneHandler for SiblingHarnessHandler {
            fn handle(
                &self,
                topic: &str,
                payload: serde_json::Value,
            ) -> std::result::Result<serde_json::Value, String> {
                assert_eq!(topic, topics::DAEMON_HARNESS);
                let mut res = susi_daemon::harness_ops::dispatch_with_authority(
                    &payload,
                    Some(&self.authority),
                )?;
                if res.get("revoked").and_then(serde_json::Value::as_bool) == Some(true) {
                    res["cancelled_runs"] = serde_json::json!(["mission-under-envelope"]);
                } else if res
                    .pointer("/result/revoked")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    res["result"]["cancelled_runs"] = serde_json::json!(["mission-under-envelope"]);
                }
                Ok(res)
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "susi-sibling-revoke-{}-{}",
            std::process::id(),
            susi_harness::operator::now_unix_ms()
        ));
        let bus_root = dir.join("bus");

        // Offline: missing bus_root returns 0 contacted immediately with zero network wait.
        let offline = super::revoke_on_sibling_bus(&bus_root, "1001", "env-offline");
        assert_eq!(
            offline,
            super::SiblingRevokeReport {
                contacted: 0,
                revoked: false,
                cancelled_runs: Vec::new(),
            }
        );

        // Spin up a real sibling IpcPlaneBus under `<bus_root>/999999` with an isolated authority.
        let authority = Arc::new(DelegationAuthority::from_secret(&[0x5Au8; 32]));
        let root_scope = DelegationScope::root(
            DelegationHop::new("daemon-driver", "root").unwrap(),
            ["harness:run".to_string()],
            5_000_000,
            Some(60_000),
            DelegationLimits::default(),
        )
        .unwrap();
        let issued = authority.issue(&root_scope).unwrap();
        let env_id = issued.id.clone().expect("signed envelope id");

        let sibling_bus = IpcPlaneBus::with_rendezvous(bus_root.join("999999"));
        sibling_bus.register_prefix(
            "daemon.",
            Arc::new(SiblingHarnessHandler {
                authority: Arc::clone(&authority),
            }),
        );

        // Self pid `999999` is skipped so a CLI never loops back into its own prefix handler.
        let skipped_self = super::revoke_on_sibling_bus(&bus_root, "999999", &env_id);
        assert_eq!(skipped_self.contacted, 0);
        assert!(authority.admit(&issued).is_ok());

        // From CLI pid `1001`, sibling `999999` is contacted and revokes `env_id`.
        let live = super::revoke_on_sibling_bus(&bus_root, "1001", &env_id);
        assert_eq!(live.contacted, 1);
        assert!(live.revoked);
        assert_eq!(live.cancelled_runs, vec!["mission-under-envelope"]);
        assert!(
            authority.admit(&issued).is_err(),
            "revoked envelope must be refused on subsequent admission in sibling daemon"
        );

        let idempotent = super::revoke_on_sibling_bus(&bus_root, "1001", &env_id);
        assert_eq!(idempotent.contacted, 1);
        assert!(idempotent.revoked);

        let unknown =
            super::revoke_on_sibling_bus(&bus_root, "1001", "00000000000000000000000000000000");
        assert_eq!(unknown.contacted, 1);
        assert!(!unknown.revoked);
        assert!(unknown.cancelled_runs.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
