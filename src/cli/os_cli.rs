//! `susi os` — the operating-system view of the substrate in one shot:
//! consensus state (term / leader / decisions from the replicated
//! ledger), the in-process swarm host (orchestrator / workers /
//! identities), the leaf-service process table, and the verified peer
//! roster. This is the operator's answer to "is the OS for agents
//! healthy right now?" — ledger/peers are persisted kernel state (accurate
//! even when the daemon is down); swarm host is the in-process composition.

use anyhow::Result;
use clap::Subcommand;
use std::path::Path;
use susi_core::{commit_log, service_table};

#[derive(Debug, clap::Subcommand)]
pub enum OsCommands {
    /// Full substrate status: consensus, services, peers (default)
    Status {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
    },
    /// Remove stray heavyweight files flagged in `~/.susi/bin` and
    /// report reclaimed space
    Clean,
    /// Discover the local AI ecosystem: hardware, runtimes, models, agents,
    /// frameworks, MCP servers, and running AI processes
    Ecosystem {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Exit unsuccessfully when installed components are unhealthy
        #[arg(long)]
        doctor: bool,
    },
    /// Live runtime view: AI processes by owner (SUSI vs external), managed
    /// models with observed residency, and per-process GPU memory
    Runtime {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Explain the live local-versus-cloud inference placement decision
    Route {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Required model capability, for example `vision`
        #[arg(long)]
        requires: Option<String>,
        /// Maximum acceptable request-cost hint in USD
        #[arg(long)]
        max_cost: Option<f64>,
        /// Prohibit placement on cloud providers
        #[arg(long)]
        no_cloud: bool,
    },
    /// Re-admit a repaired cloud provider after routing quarantine
    RouteReset {
        /// Exact provider id shown by `susi os route --json`
        provider: String,
    },
    /// Unified lifecycle facade for local AI ecosystem components
    Manage {
        #[command(subcommand)]
        resource: OsManageCommands,
    },
    /// Explicit cloud/local host provision. The 30s OS tick only
    /// version-probes operator CLIs (`probe_all`); list/apply never run
    /// unsolicited — that is the OS contract, not a silent cloud API.
    Provision {
        #[command(subcommand)]
        action: OsProvisionAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum OsProvisionAction {
    /// PATH version-probe of kubectl/docker/aws/gcloud/az (same as the 30s tick)
    Probe {
        #[arg(long)]
        json: bool,
    },
    /// Live inventory (`kubectl get nodes`, `docker ps`, cloud instance list)
    List {
        /// kubernetes|docker|aws|gcp|azure
        kind: String,
        #[arg(long)]
        json: bool,
    },
    /// Apply a Kubernetes manifest (`kubectl apply -f`). Other kinds refuse.
    Apply {
        /// kubernetes (kubectl-only)
        kind: String,
        /// Manifest file path
        manifest: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum OsManageCommands {
    /// Manage a supervised SUSI service
    Service {
        #[command(subcommand)]
        action: OsServiceAction,
    },
    /// Manage a local or cloud model definition
    Model {
        #[command(subcommand)]
        action: OsModelAction,
    },
    /// Manage an MCP tool server
    Mcp {
        #[command(subcommand)]
        action: OsMcpAction,
    },
    /// Manage an external execution agent
    Agent {
        #[command(subcommand)]
        action: OsAgentAction,
    },
    /// Manage an agent framework
    Framework {
        #[command(subcommand)]
        action: OsFrameworkAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum OsServiceAction {
    Status,
    Start {
        name: String,
    },
    Stop {
        name: String,
    },
    Restart {
        name: String,
    },
    Logs {
        name: String,
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Debug, Subcommand)]
pub enum OsModelAction {
    List,
    Doctor {
        name: Option<String>,
    },
    /// Download a GGUF model from an HTTPS URL into SUSI-managed storage
    Install {
        source: String,
    },
    /// Remove a GGUF model and paired tokenizer from SUSI-managed storage
    Uninstall {
        name: String,
    },
    /// Load a model into the daemon's inference cache (warm start)
    Load {
        name: String,
    },
    /// Evict a model from the daemon's inference cache, freeing its memory
    Unload {
        name: String,
    },
    Prefer {
        name: String,
    },
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    Reset {
        name: String,
    },
    Local,
}

#[derive(Debug, Subcommand)]
pub enum OsMcpAction {
    List,
    Doctor {
        name: Option<String>,
    },
    /// Install/admit a managed MCP server into the active configuration
    Install {
        name: String,
    },
    /// Uninstall/remove a managed MCP server from the active configuration
    Uninstall {
        name: String,
    },
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    Reset {
        name: String,
    },
    Status,
}

#[derive(Debug, Subcommand)]
pub enum OsAgentAction {
    List,
    Doctor {
        name: Option<String>,
    },
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    Reset {
        name: String,
    },
    Run {
        name: String,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    Tasks,
    Cancel {
        task_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum OsFrameworkAction {
    List,
    Doctor {
        name: Option<String>,
    },
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    Reset {
        name: String,
    },
    Run {
        name: String,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    Tasks,
    Cancel {
        task_id: String,
    },
}

pub fn execute(action: Option<OsCommands>, top_json: bool, workspace: &Path) -> Result<()> {
    match action.unwrap_or(OsCommands::Status { json: false }) {
        OsCommands::Status { json } => status(json || top_json),
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
    }
}

fn provision(action: OsProvisionAction, top_json: bool) -> Result<()> {
    match action {
        OsProvisionAction::Probe { json } => {
            let inv = susi_vendor_cloud::probe_all();
            if json || top_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "tick_contract": "probe_all_only",
                        "inventory": inv.iter().map(|c| serde_json::json!({
                            "kind": c.kind,
                            "tool": c.tool,
                            "available": c.available,
                            "summary": c.summary,
                        })).collect::<Vec<_>>(),
                    }))?
                );
            } else {
                println!("provision probe (tick contract: version-only, no list/apply)");
                for c in inv {
                    let flag = if c.available { "ok" } else { "missing" };
                    println!(
                        "  {kind} ({tool}): {flag} — {summary}",
                        kind = c.kind,
                        tool = c.tool,
                        summary = c.summary
                    );
                }
            }
        }
        OsProvisionAction::List { kind, json } => {
            let kind = susi_vendor_cloud::CloudKind::parse(&kind).map_err(anyhow::Error::msg)?;
            let out = susi_vendor_cloud::list_nodes(kind).map_err(anyhow::Error::msg)?;
            if json || top_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "kind": kind.as_str(),
                        "explicit": true,
                        "nodes": out,
                    }))?
                );
            } else {
                println!("{out}");
            }
        }
        OsProvisionAction::Apply { kind, manifest } => {
            let kind = susi_vendor_cloud::CloudKind::parse(&kind).map_err(anyhow::Error::msg)?;
            let text = std::fs::read_to_string(&manifest)?;
            let out = susi_vendor_cloud::apply_manifest(kind, &text).map_err(anyhow::Error::msg)?;
            println!("{out}");
        }
    }
    Ok(())
}

fn route_reset(provider: &str, json: bool, workspace: &Path) -> Result<()> {
    let provider = provider.trim();
    if !susi_gemi::susi_core::plane_bus::gemi::valid_provider_id(provider) {
        anyhow::bail!("provider must be 1-256 printable non-whitespace ASCII characters");
    }
    let cleared =
        susi_gemi::susi_core::plane_bus::gemi::ModelManager::clear_provider_cooldown(provider)
            .map_err(anyhow::Error::msg)?;
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
    susi_gemi::http_provider::register_configured_cloud_endpoints(
        susi_gemi::susi_core::registry::CapabilityRegistry::global(),
    );
    let providers = susi_gemi::susi_core::registry::CapabilityRegistry::global().list_providers();
    let decision = susi_gemi::routing::InferenceRouter::plan_placement_for(
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
                OsModelAction::Install { source } => {
                    let installed = susi_gemi::ModelManager::install_model_foreground(&source)
                        .map_err(anyhow::Error::msg)?;
                    println!("{installed}");
                    return Ok(());
                }
                OsModelAction::Uninstall { name } => {
                    uninstall_managed_model(&name)?;
                    return Ok(());
                }
                OsModelAction::Load { name } => {
                    // Large models can take minutes to read and place.
                    let result = super::os_runtime::daemon_request(
                        "POST",
                        "/runtime/models/load",
                        Some(&serde_json::json!({ "model": name })),
                        1800,
                    )
                    .map_err(anyhow::Error::msg)?;
                    println!(
                        "loaded {}",
                        result["loaded"].as_str().unwrap_or(name.as_str())
                    );
                    return Ok(());
                }
                OsModelAction::Unload { name } => {
                    let result = super::os_runtime::daemon_request(
                        "POST",
                        "/runtime/models/unload",
                        Some(&serde_json::json!({ "model": name })),
                        30,
                    )
                    .map_err(anyhow::Error::msg)?;
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
                OsModelAction::Local => model_cli::ModelCommands::Local,
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

fn uninstall_managed_model(name: &str) -> Result<()> {
    let requested = std::path::Path::new(name);
    if requested.components().count() != 1
        || requested.extension().and_then(|ext| ext.to_str()) != Some("gguf")
    {
        anyhow::bail!("model uninstall accepts one managed .gguf filename, not a path");
    }
    let models_dir = susi_gemi::ModelManager::get_models_dir()
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("resolve managed model directory: {error}"))?;
    let canonical = models_dir
        .join(requested)
        .canonicalize()
        .map_err(|error| anyhow::anyhow!("managed model `{name}` not found: {error}"))?;
    if canonical.parent() != Some(models_dir.as_path()) || !canonical.is_file() {
        anyhow::bail!("refusing to remove a model outside SUSI-managed storage");
    }
    let daemon_running =
        susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir()).is_some();
    if daemon_running {
        let loaded = super::os_runtime::daemon_loaded_models().map_err(|error| {
            anyhow::anyhow!("cannot confirm `{name}` is not loaded by the daemon: {error}")
        })?;
        let canonical_str = canonical.to_string_lossy();
        if loaded["models"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| m["path"].as_str() == Some(canonical_str.as_ref()))
        {
            anyhow::bail!(
                "`{name}` is loaded by the daemon; run `susi os manage model unload {name}` first"
            );
        }
    }
    if susi_gemi::ModelManager::get_selected_model(None)
        .as_deref()
        .is_some_and(|selected| selected == name || selected == canonical.to_string_lossy())
    {
        anyhow::bail!("`{name}` is selected; prefer another model before uninstalling it");
    }
    std::fs::remove_file(&canonical)
        .map_err(|error| anyhow::anyhow!("remove {}: {error}", canonical.display()))?;
    let stem = requested
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let tokenizer = models_dir.join(format!("{stem}.tokenizer.json"));
    let tokenizer_removed = match std::fs::remove_file(&tokenizer) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(anyhow::anyhow!(
                "model removed, but paired tokenizer {} could not be removed: {error}",
                tokenizer.display()
            ));
        }
    };
    println!(
        "uninstalled {}{}",
        canonical.display(),
        if tokenizer_removed {
            " and its paired tokenizer"
        } else {
            ""
        }
    );
    Ok(())
}

fn command_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn command_version(path: &Path) -> Option<String> {
    std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            let text = if output.stdout.is_empty() {
                &output.stderr
            } else {
                &output.stdout
            };
            String::from_utf8_lossy(text)
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|version| !version.is_empty())
}

fn runtime_inventory() -> Vec<serde_json::Value> {
    [
        ("cuda", "nvidia-smi"),
        ("cuda-toolkit", "nvcc"),
        ("ollama", "ollama"),
        ("llama.cpp", "llama-server"),
        ("vllm", "vllm"),
        ("python", "python3"),
        ("uv", "uv"),
        ("docker", "docker"),
        ("podman", "podman"),
        ("node", "node"),
        ("npx", "npx"),
        ("rust", "cargo"),
    ]
    .into_iter()
    .map(|(id, command)| {
        let path = command_path(command);
        serde_json::json!({
            "id": id,
            "command": command,
            "installed": path.is_some(),
            "path": path,
            "version": path.as_deref().and_then(command_version),
        })
    })
    .collect()
}

fn ecosystem(json: bool, doctor: bool, workspace: &Path) -> Result<()> {
    let hardware = susi_gemi::hardware::HardwareProfiler::get_profile();
    let runtimes = runtime_inventory();
    let models = susi_core::plane_bus::gemi::ModelManager::list_models(workspace);
    let model_count = models.as_array().map_or(0, Vec::len);
    let agent_catalog =
        susi_agents::external::catalog(susi_agents::external::CatalogKind::Execution)?;
    let framework_catalog =
        susi_agents::external::catalog(susi_agents::external::CatalogKind::Framework)?;
    let agent_manager = susi_agents::external::AgentManager::new(workspace)?;
    let framework_manager = susi_agents::external::AgentManager::frameworks(workspace)?;
    let agents: Vec<_> = agent_catalog
        .into_iter()
        .map(|definition| {
            let readiness = agent_manager
                .adapter(&definition.id)
                .and_then(|adapter| adapter.preflight());
            serde_json::json!({
                "definition": definition,
                "ready": readiness.is_ok(),
                "detail": match readiness { Ok(detail) => detail, Err(error) => error.to_string() },
            })
        })
        .collect();
    let frameworks: Vec<_> = framework_catalog
        .into_iter()
        .map(|definition| {
            let readiness = framework_manager
                .adapter(&definition.id)
                .and_then(|adapter| adapter.preflight());
            serde_json::json!({
                "definition": definition,
                "ready": readiness.is_ok(),
                "detail": match readiness { Ok(detail) => detail, Err(error) => error.to_string() },
            })
        })
        .collect();
    let mcp = susi_tools::LeadingMcpManager::new(workspace)?.status()?;
    let runtime = super::os_runtime::view();
    let processes = runtime["processes"].as_array().cloned().unwrap_or_default();
    let installed_runtimes = runtimes
        .iter()
        .filter(|runtime| runtime["installed"].as_bool() == Some(true))
        .count();
    let unhealthy_installed = runtimes
        .iter()
        .filter(|runtime| {
            runtime["installed"].as_bool() == Some(true) && runtime["version"].is_null()
        })
        .count();
    let body = serde_json::json!({
        "hardware": hardware,
        "runtimes": runtimes,
        "models": models,
        "agents": agents,
        "frameworks": frameworks,
        "mcp_servers": mcp,
        "running_ai_processes": processes,
        "summary": {
            "installed_runtimes": installed_runtimes,
            "unhealthy_installed_runtimes": unhealthy_installed,
            "models": model_count,
            "agents": agents.len(),
            "frameworks": frameworks.len(),
            "mcp_servers": mcp.len(),
            "running_ai_processes": processes.len(),
        },
        "control": {
            "runtime": "susi os runtime [--json]",
            "services": "susi os manage service <status|start|stop|restart|logs>",
            "models": "susi os manage model <list|doctor|install|uninstall|load|unload|configure|reset|prefer|local>",
            "agents": "susi os manage agent <list|doctor|configure|reset|run|tasks|cancel>",
            "frameworks": "susi os manage framework <list|doctor|configure|reset|run|tasks|cancel>",
            "mcp": "susi os manage mcp <list|doctor|install|uninstall|configure|reset|status>",
            "privacy": "susi privacy <status|mode|grant|revoke>",
        }
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("SUSI OS — local AI ecosystem");
        println!(
            "hardware:   {} CPU(s), {}GB RAM, {}",
            hardware.cpus, hardware.ram_gb, hardware.gpu_info
        );
        println!(
            "runtimes:   {installed_runtimes}/{} installed",
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
        println!("models:     {model_count} discovered");
        for model in models.as_array().into_iter().flatten() {
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
        println!("agents:     {} catalogued", agents.len());
        println!("frameworks: {} catalogued", frameworks.len());
        println!("MCP:        {} catalogued/configured", mcp.len());
        println!(
            "processes:  {} AI-related process(es) running",
            processes.len()
        );
        println!();
        println!("Manage: `susi os manage <service|model|mcp|agent|framework> …`");
    }
    if doctor && unhealthy_installed > 0 {
        anyhow::bail!("{unhealthy_installed} installed AI runtime(s) failed their version probe");
    }
    Ok(())
}

/// Hygiene check: the credential files consensus depends on
/// (`cluster.key`, `node.key`, `api_token`) are created 0600, but an
/// operator `chmod` or a bad umask can loosen them silently — and a
/// world-readable cluster.key hands every local user full cluster
/// membership. Surfacing it in the status view turns an invisible
/// misconfiguration into an actionable warning.
#[cfg(unix)]
fn credential_warnings() -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = susi_paths::SusiDirs::config_dir();
    let mut warnings = Vec::new();
    for name in ["cluster.key", "node.key", "api_token"] {
        let path = dir.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue; // absent is a bootstrap state, not a perms issue
        };
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            warnings.push(format!(
                "{name} is group/world-readable (mode {mode:04o}) — run `chmod 600 {}`",
                path.display()
            ));
        }
    }
    warnings
}

#[cfg(not(unix))]
fn credential_warnings() -> Vec<String> {
    Vec::new()
}

fn status(json: bool) -> Result<()> {
    let state = commit_log::replay();
    let term = commit_log::load_term();
    let services = service_table::status();
    let peers = load_verified_peers();
    let banned_count = load_banned_count();
    let up = services.iter().filter(|s| s.up).count();
    // Ledger flow: raw record count + the newest commit's age — an
    // operator watching replication health needs to see the log is
    // moving, not just that consensus state parses.
    let records = commit_log::load();
    let last_commit_age = records.iter().map(|r| r.committed_at).max().map(|newest| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(newest)
    });
    // A committed member_remove naming this node stands it down.
    let evicted = susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists();

    if json {
        let daemon =
            susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
        let swarm_host = susi_daemon::composition::swarm_host_snapshot();
        let body = serde_json::json!({
            "node_id": susi_config::cluster_key::wire_node_id(),
            "evicted": evicted,
            "swarm_host": swarm_host,
            "consensus": {
                "term": state.term.max(term.term),
                "leader": leader_display(&state, &term),
                "decisions": state.decisions,
                "anomalies": state.anomalies,
                "coordinators": state.coordinators,
            },
            "ledger": {
                "records": records.len(),
                "last_commit_age_secs": last_commit_age,
                "snapshot": commit_log::load_snapshot().map(|s| serde_json::json!({
                    "created_at": s.created_at,
                    "coordinators": s.high_water.len(),
                    "archived_records": s.decisions,
                })),
            },
            "daemon": daemon.as_ref().map(|d| serde_json::json!({
                "pid": d.pid, "substrate_home": d.substrate_home,
            })),
            "storage": disk_free(&susi_paths::SusiDirs::substrate_home())
                .map(|(avail, total)| serde_json::json!({
                    "available_bytes": avail, "total_bytes": total,
                })),
            "endpoints": endpoint_probes().iter().map(|(name, port, up)| {
                serde_json::json!({ "name": name, "port": port, "up": up })
            }).chain(std::iter::once(serde_json::json!({
                "name": "a2a-udp", "port": udp_port(),
                "up": null,
                "host_contract": true,
            }))).chain(std::iter::once(serde_json::json!({
                "name": "gossip-udp", "port": gossip_port(),
                "up": null,
                "host_contract": false,
                "note": "swarm-internal; not in ports::ALL (9092 is A2A discovery)",
            }))).collect::<Vec<_>>(),
            "substrate_usage": substrate_usage().iter().take(8).map(|(name, bytes)| {
                serde_json::json!({ "entry": name, "bytes": bytes })
            }).collect::<Vec<_>>(),
            "services": services.iter().map(|s| serde_json::json!({
                "name": s.name, "port": s.port, "pid": s.pid,
                "restarts": s.restarts, "up": s.up, "external": s.external,
                "stopped": s.stopped, "rss": s.rss(),
            })).collect::<Vec<_>>(),
            "peers": peers.iter().map(|p| serde_json::json!({
                "node_id": &p.node_id, "address": &p.address,
                "trust_score": p.trust_score, "fresh": p.fresh(),
                "reachable": p.probe(),
            })).collect::<Vec<_>>(),
            "banned_peers": banned_count,
            "key_epoch": susi_config::cluster_key::cluster_key()
                .map(|k| susi_config::cluster_key::key_fingerprint(&k)[..12].to_string()),
            "staged_key_epoch": susi_config::cluster_key::staged_key()
                .map(|k| susi_config::cluster_key::key_fingerprint(&k)[..12].to_string()),
            "warnings": credential_warnings()
                .into_iter()
                .chain(stray_bin_warnings())
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!("SUSI OS — substrate status");
    let swarm_host = susi_daemon::composition::swarm_host_snapshot();
    println!(
        "swarm host:  {} — {} worker(s), {} busy, {} identit{}",
        swarm_host
            .get("orchestrator_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unwired"),
        swarm_host
            .get("workers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        swarm_host.get("busy").and_then(|v| v.as_u64()).unwrap_or(0),
        swarm_host
            .get("identities")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        if swarm_host
            .get("identities")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            == 1
        {
            "y"
        } else {
            "ies"
        },
    );
    println!("node:        {}", susi_config::cluster_key::wire_node_id());
    // Key epoch: the current key's fingerprint prefix, plus any staged
    // next-epoch key awaiting its committed rekey record — operators
    // comparing `susi os` across nodes can see rotation drift at a
    // glance without the key itself ever being printed.
    let key_epoch = susi_config::cluster_key::cluster_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k));
    let staged_epoch = susi_config::cluster_key::staged_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k));
    if let Some(fp) = &key_epoch {
        println!(
            "key epoch:   {}{}",
            &fp[..12],
            staged_epoch
                .map(|s| format!(" — rotation to {} staged, awaiting commit", &s[..12]))
                .unwrap_or_default()
        );
    }
    let term_age = if term.term == 0 || term.updated_at == 0 {
        String::new()
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!(" (leader for {}s)", now.saturating_sub(term.updated_at))
    };
    println!(
        "consensus:   term {} / leader {}{} — {} decision(s), {} anomal{}",
        state.term.max(term.term),
        leader_display(&state, &term),
        term_age,
        state.decisions,
        state.anomalies.len(),
        if state.anomalies.len() == 1 {
            "y"
        } else {
            "ies"
        }
    );
    println!(
        "ledger:      {} record(s), last commit {}{}",
        records.len(),
        last_commit_age
            .map(|a| format!("{a}s ago"))
            .unwrap_or_else(|| "never".to_string()),
        commit_log::load_snapshot()
            .map(|s| format!(
                " — snapshot at {} ({} records archived)",
                s.created_at, s.decisions
            ))
            .unwrap_or_default()
    );
    println!("services:    {}/{} leaf services up", up, services.len());
    println!(
        "peers:       {} verified cluster member(s){}",
        peers.len(),
        if banned_count > 0 {
            format!(", {banned_count} banned")
        } else {
            String::new()
        }
    );
    if evicted {
        println!("cluster:     EVICTED — removed by a committed member_remove; standing down until a committed unban");
    }
    for warning in credential_warnings()
        .into_iter()
        .chain(stray_bin_warnings())
    {
        println!("warning:     {warning}");
    }
    let daemon = susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
    println!(
        "daemon:      {}",
        daemon
            .as_ref()
            .map(|d| format!("running (pid {})", d.pid))
            .unwrap_or_else(|| "not running".to_string())
    );
    {
        let fields: Vec<String> = endpoint_probes()
            .iter()
            .map(|(name, port, up)| format!("{name} :{port} {}", if *up { "up" } else { "DOWN" }))
            .collect();
        println!(
            "endpoints:   {} (+a2a-udp :{} · gossip-udp :{})",
            fields.join(" · "),
            udp_port(),
            gossip_port()
        );
    }
    if let Some((avail, total)) = disk_free(&susi_paths::SusiDirs::substrate_home()) {
        println!(
            "storage:     {} free / {} total on substrate_home",
            human_bytes(avail),
            human_bytes(total)
        );
    }
    let usage = substrate_usage();
    if !usage.is_empty() {
        let top: Vec<String> = usage
            .iter()
            .take(4)
            .map(|(name, bytes)| format!("{name} {}", human_bytes(*bytes)))
            .collect();
        println!("footprint:   {}", top.join(" · "));
    }
    println!();

    println!(
        "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} UP",
        "SERVICE", "PORT", "PID", "RESTARTS", "UPTIME", "RSS"
    );
    let any_external = services.iter().any(|s| s.external);
    for s in &services {
        let pid = match (s.external, s.pid) {
            (true, Some(p)) => format!("{p}*"),
            (true, None) => "ext*".to_string(),
            (false, Some(p)) => p.to_string(),
            (false, None) => "-".to_string(),
        };
        println!(
            "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} {}",
            s.name,
            s.port,
            pid,
            s.restarts,
            s.uptime(),
            s.rss(),
            if s.stopped {
                "stopped"
            } else if s.up {
                "yes"
            } else {
                "no"
            }
        );
    }
    if any_external {
        println!("* external process — bound outside daemon supervision");
    }
    println!();

    if peers.is_empty() {
        println!("no verified peers (peers.json empty — this node runs standalone)");
    } else {
        let live = peers.iter().filter(|p| p.probe()).count();
        println!("peers live:  {}/{} reachable", live, peers.len());
        println!();
        println!(
            "{:<22} {:<22} {:<7} {:<7} REACHABLE",
            "PEER", "ADDRESS", "TRUST", "FRESH"
        );
        for p in &peers {
            println!(
                "{:<22} {:<22} {:<7.2} {:<7} {}",
                p.node_id,
                p.address,
                p.trust_score,
                if p.fresh() { "yes" } else { "stale" },
                if p.probe() { "yes" } else { "no" }
            );
        }
    }

    if !state.anomalies.is_empty() {
        println!();
        println!("ledger anomalies (see `susi commits audit`):");
        for a in &state.anomalies {
            println!("  - {a}");
        }
    }
    Ok(())
}

/// Per-entry disk usage of `substrate_home`, largest first. Some
/// subsystems grow by design (model downloads, the cargo build cache,
/// rotated archives); the OS view should show where the substrate's
/// bytes actually live so an operator can spot unbounded growth rather
/// than discovering it via a full disk.
///
/// The recursive walk is bounded — an adversarial or pathological tree
/// must not stall a status command.
fn substrate_usage() -> Vec<(String, u64)> {
    const MAX_ENTRIES: usize = 250_000;
    let home = susi_paths::SusiDirs::substrate_home();
    let mut entries = 0usize;
    fn dir_size(path: &Path, entries: &mut usize) -> u64 {
        let mut total = 0u64;
        let Ok(read) = std::fs::read_dir(path) else {
            return 0;
        };
        for entry in read.flatten() {
            if *entries >= MAX_ENTRIES {
                return total;
            }
            *entries += 1;
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size(&entry.path(), entries);
            }
        }
        total
    }
    let mut usage: Vec<(String, u64)> = std::fs::read_dir(&home)
        .map(|read| {
            read.flatten()
                .map(|e| {
                    let size = e
                        .metadata()
                        .map(|m| {
                            if m.is_dir() {
                                dir_size(&e.path(), &mut entries)
                            } else {
                                m.len()
                            }
                        })
                        .unwrap_or(0);
                    (e.file_name().to_string_lossy().to_string(), size)
                })
                .collect()
        })
        .unwrap_or_default();
    usage.sort_by_key(|a| std::cmp::Reverse(a.1));
    usage
}

/// Stray heavyweight files inside `~/.susi/bin` — only the live binary
/// (`susi`/`susi.exe`) and an optional `lib/` dir belong there; an
/// orphaned `susi.rollback-*` from a manual backup can quietly hold
/// gigabytes. Flagged, never auto-deleted.
fn stray_bin_warnings() -> Vec<String> {
    const STRAY_WARN_BYTES: u64 = 256 * 1024 * 1024;
    let bin_dir = susi_paths::SusiDirs::substrate_home().join("bin");
    let mut warnings = Vec::new();
    let Ok(read) = std::fs::read_dir(&bin_dir) else {
        return warnings;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "susi" | "susi.exe" | "lib") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() && meta.len() >= STRAY_WARN_BYTES {
            warnings.push(format!(
                "stray {name} ({}) in {} — `susi os clean` removes it",
                human_bytes(meta.len()),
                bin_dir.display()
            ));
        }
    }
    // Pre-cap rotated metrics residue: the sink rotates one generation
    // at 64MiB, so a `.1` larger than that can only be residue.
    if let Some((path, len)) = susi_error::oversized_rotated_metrics() {
        warnings.push(format!(
            "oversized rotated metrics {} ({}) — `susi os clean` removes it",
            path.display(),
            human_bytes(len)
        ));
    }
    // Same residue class: the flat audit.log is dead once dated rotation
    // files exist and are newer (the fallback sink would be newer instead).
    if let Some((path, len)) = susi_error::stale_flat_audit_log() {
        warnings.push(format!(
            "stale pre-rotation audit log {} ({}) — `susi os clean` removes it",
            path.display(),
            human_bytes(len)
        ));
    }
    warnings
}

/// Remove the same files `stray_bin_warnings` flags — stray binaries in
/// `~/.susi/bin` at or above the warn threshold. Only files the warning
/// predicate already names are touched; the live binary and `lib/` are
/// never removed.
pub(crate) fn clean() -> Result<()> {
    const STRAY_WARN_BYTES: u64 = 256 * 1024 * 1024;
    let bin_dir = susi_paths::SusiDirs::substrate_home().join("bin");
    let mut reclaimed = 0u64;
    let mut removed = 0usize;
    // Same predicate as the warning: oversized rotated metrics residue —
    // independent of the bin sweep so a missing bin dir cannot skip it.
    if let Some((path, len)) = susi_error::oversized_rotated_metrics() {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {} ({})", path.display(), human_bytes(len));
                reclaimed += len;
                removed += 1;
            }
            Err(e) => eprintln!("could not remove {}: {e}", path.display()),
        }
    }
    if let Some((path, len)) = susi_error::stale_flat_audit_log() {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {} ({})", path.display(), human_bytes(len));
                reclaimed += len;
                removed += 1;
            }
            Err(e) => eprintln!("could not remove {}: {e}", path.display()),
        }
    }
    let Ok(read) = std::fs::read_dir(&bin_dir) else {
        println!(
            "cleaned {removed} file(s), reclaimed {}",
            human_bytes(reclaimed)
        );
        return Ok(());
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "susi" | "susi.exe" | "lib") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !(meta.is_file() && meta.len() >= STRAY_WARN_BYTES) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {} ({})", path.display(), human_bytes(meta.len()));
                reclaimed += meta.len();
                removed += 1;
            }
            Err(e) => eprintln!("could not remove {}: {e}", path.display()),
        }
    }
    println!(
        "cleaned {removed} file(s), reclaimed {}",
        human_bytes(reclaimed)
    );
    Ok(())
}

/// Free/total bytes on the filesystem holding `path` — a ledger or roster
/// write failing on a full disk is a substrate-level fault the OS view
/// should surface, not hide behind generic IO errors.
fn disk_free(path: &std::path::Path) -> Option<(u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // Longest-mount-point-prefix match (e.g. /home on a separate fs).
    disks
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.available_space(), d.total_space()))
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Prefer the persisted term's leader when it is ahead of what the
/// ledger alone shows — the persisted file is updated by elections even
/// when no commits followed.
fn leader_display<'a>(
    state: &'a commit_log::ClusterState,
    term: &'a commit_log::TermState,
) -> String {
    if term.term > state.term {
        if term.leader.is_empty() {
            "(none)".to_string()
        } else {
            term.leader.clone()
        }
    } else if state.leader.is_empty() {
        "(none)".to_string()
    } else {
        state.leader.clone()
    }
}

/// The persisted roster is `susi_gawd_swarm`'s type; the root crate reads
/// it structurally (node_id / address / trust_score / last_seen_secs) so the
/// OS view needs no dependency edge into the swarm plane.
struct PeerView {
    node_id: String,
    address: String,
    trust_score: f64,
    last_seen_secs: u64,
}

/// Mirrors `susi_gawd_swarm::amas::PEER_STALE_SECS` — kept as a literal so
/// the root crate keeps zero dependency edges into the swarm plane.
const PEER_STALE_SECS: u64 = 30;

impl PeerView {
    /// Whether the peer's last signed pong is inside the staleness window —
    /// the swarm's definition of quorum-eligible liveness.
    fn fresh(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_seen_secs != 0 && now.saturating_sub(self.last_seen_secs) <= PEER_STALE_SECS
    }

    /// TCP liveness probe — the persisted roster only records who was
    /// verified, not who is reachable right now. 300ms budget: peers
    /// are LAN-adjacent, so a longer wait just stalls the status view.
    fn probe(&self) -> bool {
        use std::net::ToSocketAddrs;
        let Some(addr) = self
            .address
            .to_socket_addrs()
            .ok()
            .and_then(|mut i| i.next())
        else {
            return false;
        };
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
    }
}

/// Effective UDP discovery port (canonical base + `port_offset`).
fn udp_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.udp_discovery_port())
        .unwrap_or(susi_paths::ports::UDP_DISCOVERY)
}

fn gossip_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.gossip_port())
        .unwrap_or(susi_paths::ports::effective(susi_paths::ports::GOSSIP))
}

/// Live TCP probes of the public host-contract endpoints — shared by the
/// text view's `endpoints:` line and the `--json` payload. UDP discovery
/// has no TCP probe; callers surface its port statically.
fn endpoint_probes() -> Vec<(&'static str, u16, bool)> {
    let cfg = susi_config::SusiConfig::load_global().unwrap_or_default();
    let probes: [(&str, u16); 4] = [
        ("gmcp", cfg.gmcp_port()),
        ("gemi", cfg.gemi_port()),
        ("gmcp-sse", cfg.gmcp_http_port()),
        ("a2a", cfg.a2a_http_port()),
    ];
    probes
        .iter()
        .map(|(name, port)| {
            let up = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    *port,
                ),
                std::time::Duration::from_millis(150),
            )
            .is_ok();
            (*name, *port, up)
        })
        .collect()
}

/// Count of operator-evicted members — a nonzero value means
/// `peers_banned.json` holds members blocked at handshake.
fn load_banned_count() -> usize {
    let path = susi_paths::SusiDirs::config_dir().join("peers_banned.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
        .map(|v| v.len())
        .unwrap_or(0)
}

fn load_verified_peers() -> Vec<PeerView> {
    let path = susi_paths::SusiDirs::config_dir().join("peers.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(nodes) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return Vec::new();
    };
    nodes
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            Some(PeerView {
                node_id: n.get("node_id")?.as_str()?.to_string(),
                address: n.get("address")?.as_str()?.to_string(),
                trust_score: n.get("trust_score").and_then(|t| t.as_f64()).unwrap_or(0.0),
                last_seen_secs: n
                    .get("last_seen_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            })
        })
        .collect()
}
