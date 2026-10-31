//! Zero-config substrate discovery pipeline: cloud keys, extension packs,
//! local inference engines, MCP servers, catalogs, and periodic rediscovery.
//! Swarm Cell spawning lives in [`crate::auto_discovery`].

use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use susi_gemi::models::resource_inventory::{ProbeInputs, ResourceInventory};

// Reached through susi-gemi's re-export: the same `susi-core` crate, and the
// type susi_gemi's discovery entrypoints take.
use susi_gemi::susi_core::registry::CapabilityRegistry;

/// The providers and tools the host contract answers with, registered the
/// moment the daemon's contract is up and before any slow discovery: the
/// configured cloud endpoints, the Candle fallback, and the tool registry
/// with its essential-MCP auto-link settled.
///
/// Before the contract moved ahead of discovery, the daemon's composition
/// registered all of these before it bound, so "contract up" meant
/// "providers ready". Discovery (catalog priming, local-engine probes, the
/// ecosystem scan) takes tens of seconds to minutes; registering these only
/// at its step 1b left `/v1/models`, inference and daemon pulses without a
/// single provider — not even Candle — for that whole window. This costs a
/// few seconds and runs once per daemon; the zero-config pass that follows
/// still re-registers the cloud endpoints after it prunes unhealthy
/// providers, exactly as before.
pub fn register_contract_providers() {
    use crate::boot_trace::phase;
    let registry = CapabilityRegistry::global();
    // The fallback first: it is instant, and from here on inference always
    // has somewhere to go.
    phase("contract providers: candle fallback", || {
        ensure_candle_fallback(registry)
    });
    phase("contract providers: configured cloud endpoints", || {
        susi_gemi::engines::http_provider::register_configured_cloud_endpoints(registry)
    });
    // The registry boots its tools on first use and links the essential MCP
    // servers on a one-shot; settle both so a session's `tools/list` does
    // not grow under it, and so MCP discovery below reads the linked config.
    phase("contract providers: tool registry + mcp auto-link", || {
        let _ = susi_tools::ToolRegistry::global();
        let _ = crate::service_supervision::Supervisor::global().join("mcp-auto-link");
    });
}

/// Candle (local edge) is the permanent last-resort provider.
fn ensure_candle_fallback(registry: &CapabilityRegistry) {
    if registry.get_provider("Candle (Local)").is_none() {
        registry.register_provider(susi_gemi::engines::candle_provider::CandleProvider);
    }
}

/// Universal Autonomous Substrate Bootstrapper
/// Implements Pillar 8 (Autonomous Provisioning) by triggering zero-config
/// capability discovery across the local system — packs, MCP, models, peers,
/// and local engines are primed when host prerequisites are already present.
pub async fn bootstrap_zero_config_substrate() {
    let registry = CapabilityRegistry::global();
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Initiating Zero-Config Substrate Discovery...");
    }

    // Keys first so MCP/model/peer preflights see ~/.susi/cloud.env.

    // 0. Extension packs: seed ~/.susi/extensions/default, auto-discover packs.
    match crate::susi_sandbox::extensions::ensure_extensions_substrate() {
        Ok(pack) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!(
                    "[BOOTSTRAP] Extension pack `{}` ready at {}",
                    pack.id,
                    pack.root.display()
                );
            }
        }
        Err(e) => {
            eprintln!("[BOOTSTRAP] Extension pack seed skipped: {}", e);
        }
    }

    use crate::boot_trace::{finished, phase};

    // 0b. Auto-enable leading MCP / prefer coding models / admit ready peers.
    phase("bootstrap: prime catalogs", || prime_catalogs(&substrate));

    // 0c. Awaken local inference daemons already installed on PATH.
    phase("bootstrap: awaken local engines", try_awaken_local_engines);

    // Drop HTTP providers that went unhealthy since the last pass so a killed
    // Ollama/vLLM does not keep winning routing. Candle is permanent fallback.
    let started = std::time::Instant::now();
    prune_unhealthy_providers(registry).await;
    finished("bootstrap: prune unhealthy providers", started);

    // 0d. Hardware + installed local-AI ecosystem inventory (read-only scan).
    let started = std::time::Instant::now();
    let _ = tokio::task::spawn_blocking(local_ecosystem_report).await;
    finished("bootstrap: local ecosystem report", started);

    // 1. Probe for Local Model Inference Engines (Ollama, vLLM, llama.cpp, etc.)
    //    plus any configured OpenAI-compat inference_endpoints bases (open admission).
    let started = std::time::Instant::now();
    susi_gemi::engines::http_provider::auto_discover_local_engines(registry).await;
    finished("bootstrap: local engines", started);

    // 1b. Register configured cloud + local-with-model endpoints when keys/models present.
    phase("bootstrap: configured cloud endpoints", || {
        susi_gemi::engines::http_provider::register_configured_cloud_endpoints(registry)
    });

    // 2. Discover and Provision MCP Tools (any entry in ~/.susi/mcp_config.json).
    phase(
        "bootstrap: mcp discovery",
        susi_gmcp::mcp_wrapper::auto_discover_mcp,
    );

    // 2b. MCP servers that expose chat/LLM tools → inference Providers so they
    // join cloud prefer / slow-local escalation (not tools-only).
    phase("bootstrap: mcp inference providers", || {
        susi_gemi::engines::mcp_provider::register_mcp_inference_providers(registry)
    });

    // 3. Fallback: Ensure Candle (Local Edge) is always provisioned
    phase("bootstrap: candle fallback", || {
        ensure_candle_fallback(registry)
    });

    // 3b. Local embedder: susi-gmcp self-registers through
    // the shared IPC rendezvous, so `gemi.infer.embed` and /v1/embeddings
    // work with zero external configuration.
    phase(
        "bootstrap: local embed provider",
        susi_gmcp::embed_provider::register_local_embed_provider,
    );

    // 4. Background weight priming (hardware-optimal ladder) — idempotent.
    susi_gemi::models::ModelManager::spawn_background_hardware_model_provisioner(&substrate);

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Capability Registry Loaded:");
        eprintln!(" - Providers: {:?}", registry.list_providers());
        eprintln!(" - Tools: {:?}", registry.list_tools());
    }
}

/// Hardware profile plus every installed local-AI engine, runtime and model
/// store, with the GPU backends they could use. Persisted to
/// `~/.susi/local_ecosystem.json` so `susi ecosystem` and the UI see what the
/// last startup found. Detection only: nothing is started or configured.
pub fn local_ecosystem_report() -> serde_json::Value {
    local_ecosystem_report_at(&susi_paths::SusiDirs::config_dir())
}

/// Build and persist the production local-host report at an explicit location.
/// Keeping the destination injectable lets the production composition path be
/// exercised hermetically without writing into the caller's SUSI instance.
pub fn local_ecosystem_report_at(config_dir: &Path) -> serde_json::Value {
    let eco =
        susi_gemi::models::local_ecosystem::scan(&susi_gemi::models::local_ecosystem::HostProbe);
    let profile = susi_gemi::models::hardware::HardwareProfiler::get_profile();
    let models_dir = susi_gemi::models::ModelManager::get_models_dir();
    let models = std::env::current_dir()
        .ok()
        .map(|workspace| susi_gemi::models::ModelManager::list_models(&workspace))
        .map(|found| {
            found
                .into_iter()
                .map(|model| model.model_id().to_string())
                .collect()
        });
    let runtimes = Some(
        eco.engines
            .iter()
            .filter(|engine| engine.installed)
            .map(|engine| engine.id.clone())
            .collect(),
    );
    let inventory = ResourceInventory::from_probes(ProbeInputs {
        cpu_cores: (profile.cpus > 0).then_some(profile.cpus as u32),
        ram_bytes: (profile.ram_gb > 0)
            .then_some((profile.ram_gb as u64).saturating_mul(1024 * 1024 * 1024)),
        gpu_name: profile
            .acceleration_active
            .then_some(profile.gpu_info.clone()),
        vram_bytes: (profile.acceleration_active && profile.gpu_vram_gb > 0)
            .then_some((profile.gpu_vram_gb as u64).saturating_mul(1024 * 1024 * 1024)),
        disk_free_bytes: {
            let free =
                susi_gemi::models::hardware::HardwareProfiler::get_free_disk_bytes(&models_dir);
            (free > 0).then_some(free)
        },
        models,
        runtimes,
        now_unix: None,
    });
    let report = serde_json::json!({
        "kind": "local_ecosystem",
        "hardware": profile.report_json(),
        "resource_inventory": inventory,
        "engines": eco.engines,
        "accelerators": eco.accelerators,
    });
    let path = config_dir.join("local_ecosystem.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()),
    );
    if std::env::var("SUSI_VERBOSE").is_ok() {
        let found: Vec<_> = eco
            .engines
            .iter()
            .filter(|e| e.installed)
            .map(|e| e.id.as_str())
            .collect();
        eprintln!("[BOOTSTRAP] Local AI ecosystem: {found:?}");
    }
    report
}

#[cfg(test)]
mod resource_inventory_tests {
    use super::local_ecosystem_report_at;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn vc_201_041_production_profile_uses_inventory() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after epoch")
            .as_nanos();
        let config_dir = std::env::temp_dir().join(format!("susi-vc-201-041-{nonce}"));
        std::fs::create_dir(&config_dir).expect("test config directory should be unique");

        let report = local_ecosystem_report_at(&config_dir);
        let inventory = report
            .get("resource_inventory")
            .and_then(serde_json::Value::as_object)
            .expect("production report must publish resource inventory");
        assert!(inventory.contains_key("captured_unix"));
        assert!(inventory.contains_key("cpu_cores"));
        assert!(inventory.contains_key("ram_bytes"));
        assert!(inventory.contains_key("vram_bytes"));
        assert!(inventory.contains_key("disk_free_bytes"));
        assert!(config_dir.join("local_ecosystem.json").is_file());

        std::fs::remove_dir_all(config_dir).expect("test report directory should be removable");
    }

    #[test]
    fn vc_201_041_mastery_production_hardware_section_is_observed() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after epoch")
            .as_nanos();
        let config_dir = std::env::temp_dir().join(format!("susi-vc-201-041m-{nonce}"));
        std::fs::create_dir(&config_dir).expect("test config directory should be unique");

        let report = local_ecosystem_report_at(&config_dir);
        let hardware = report
            .get("hardware")
            .and_then(serde_json::Value::as_object)
            .expect("production report must publish a hardware section");

        // Every published hardware observation carries when it was taken
        // and where it came from.
        assert!(hardware.contains_key("observed_unix"));
        assert!(hardware.contains_key("provenance"));

        // Fields whose zero can only mean "the probe failed" must never
        // publish a bare 0 as if it were measured — unmeasured is null.
        for key in ["cpus", "ram_gb", "gpu_vram_gb", "disk_gb"] {
            assert_ne!(
                hardware.get(key).and_then(serde_json::Value::as_u64),
                Some(0),
                "{key} must be a real measurement or null, never a bare 0"
            );
        }

        std::fs::remove_dir_all(config_dir).expect("test report directory should be removable");
    }
}

/// The catalogs [`prime_selected_catalogs`] can admit, each primed on its own.
///
/// A command primes only the catalogs whose state its output (or its work)
/// reads. Priming a catalog costs real host probes — the frameworks catalog
/// alone spawns one `python3 -c "import …"` per adapter — so a command that
/// reads none of them primes nothing. Nothing is cached across processes:
/// every catalog a command needs is primed fresh, in that command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Catalogs {
    /// Leading MCP auto-enable: writes `mcp_config.json`.
    pub leading_mcp: bool,
    /// Coding-model auto-prefer: writes `preferred_coding_model.txt` and
    /// `selected_model_override.txt`.
    pub coding_models: bool,
    /// Execution-agent readiness: `execution-agents/ready.json` plus the
    /// agent capabilities registered in the `CapabilityRegistry`.
    pub execution_agents: bool,
    /// Agent-framework readiness: `agent-engines/ready.json` plus the same
    /// capability registration.
    pub frameworks: bool,
}

impl Catalogs {
    /// Prime nothing.
    pub const NONE: Self = Self {
        leading_mcp: false,
        coding_models: false,
        execution_agents: false,
        frameworks: false,
    };
    /// Every catalog: what the daemon, `susi auto prime` and every command
    /// not yet classified prime.
    pub const ALL: Self = Self {
        leading_mcp: true,
        coding_models: true,
        execution_agents: true,
        frameworks: true,
    };
    /// Only leading MCP auto-enable.
    pub const LEADING_MCP: Self = Self {
        leading_mcp: true,
        ..Self::NONE
    };
    /// Only coding-model auto-prefer.
    pub const CODING_MODELS: Self = Self {
        coding_models: true,
        ..Self::NONE
    };
    /// Execution agents and agent frameworks (the capability-registering scan).
    pub const AGENTS: Self = Self {
        execution_agents: true,
        frameworks: true,
        ..Self::NONE
    };

    /// Both sets' catalogs.
    pub const fn union(self, other: Self) -> Self {
        Self {
            leading_mcp: self.leading_mcp || other.leading_mcp,
            coding_models: self.coding_models || other.coding_models,
            execution_agents: self.execution_agents || other.execution_agents,
            frameworks: self.frameworks || other.frameworks,
        }
    }

    /// No catalog selected.
    pub const fn is_empty(self) -> bool {
        !(self.leading_mcp || self.coding_models || self.execution_agents || self.frameworks)
    }

    /// Every catalog selected.
    pub const fn is_all(self) -> bool {
        self.leading_mcp && self.coding_models && self.execution_agents && self.frameworks
    }

    /// The boot-trace phase of each selected catalog, in prime order.
    pub fn phases(self) -> Vec<&'static str> {
        [
            (self.leading_mcp, PRIME_PHASE_LEADING_MCP),
            (self.coding_models, PRIME_PHASE_CODING_MODELS),
            (self.execution_agents, PRIME_PHASE_EXECUTION_AGENTS),
            (self.frameworks, PRIME_PHASE_FRAMEWORKS),
        ]
        .into_iter()
        .filter_map(|(on, label)| on.then_some(label))
        .collect()
    }
}

/// Boot-trace phase of the leading MCP auto-enable step.
pub const PRIME_PHASE_LEADING_MCP: &str = "prime: leading mcp";
/// Boot-trace phase of the coding-model auto-prefer step.
pub const PRIME_PHASE_CODING_MODELS: &str = "prime: coding models";
/// Boot-trace phase of the execution-agent readiness scan.
pub const PRIME_PHASE_EXECUTION_AGENTS: &str = "prime: execution agents";
/// Boot-trace phase of the agent-framework readiness scan.
pub const PRIME_PHASE_FRAMEWORKS: &str = "prime: frameworks";

/// Auto-admit host-ready ecosystem components (MCP, coding models, peers).
/// Persists a glass-box report under `~/.susi/last_auto_prime.json`.
pub fn prime_catalogs(substrate: &Path) -> serde_json::Value {
    prime_selected_catalogs(substrate, Catalogs::ALL)
}

/// Prime only `catalogs`, each one fresh and as its own boot-trace phase
/// ([`Catalogs::phases`]), in the same order a full prime runs them.
///
/// The glass-box report `last_auto_prime.json` is written only by a full
/// prime: a partial one would report zero for every catalog it skipped.
/// A partial prime returns a report of just the catalogs it primed.
pub fn prime_selected_catalogs(substrate: &Path, catalogs: Catalogs) -> serde_json::Value {
    use crate::boot_trace::phase;
    let mut mcp_enabled: Vec<String> = Vec::new();
    let mut preferred_model: Option<String> = None;
    let mut agents_ready = 0usize;
    let mut frameworks_ready = 0usize;

    if catalogs.leading_mcp {
        mcp_enabled = phase(PRIME_PHASE_LEADING_MCP, || prime_leading_mcp(substrate));
    }
    if catalogs.coding_models {
        preferred_model = phase(PRIME_PHASE_CODING_MODELS, prime_coding_models);
    }
    if catalogs.execution_agents {
        agents_ready = phase(PRIME_PHASE_EXECUTION_AGENTS, || {
            prime_agent_kind(substrate, susi_agents::external::CatalogKind::Execution)
        });
    }
    if catalogs.frameworks {
        frameworks_ready = phase(PRIME_PHASE_FRAMEWORKS, || {
            prime_agent_kind(substrate, susi_agents::external::CatalogKind::Framework)
        });
    }

    // Seeds the extension pack, as every prime always has.
    let pack = crate::susi_sandbox::extensions::active_pack();
    if !catalogs.is_all() {
        let mut report = serde_json::json!({
            "kind": "auto_prime_partial",
            "active_pack": pack.id,
            "pack_root": pack.root,
            "primed": catalogs.phases(),
        });
        if catalogs.leading_mcp {
            report["mcp_enabled"] = serde_json::json!(mcp_enabled);
        }
        if catalogs.coding_models {
            report["preferred_coding_model"] = serde_json::json!(preferred_model);
        }
        if catalogs.execution_agents {
            report["agents_ready"] = serde_json::json!(agents_ready);
        }
        if catalogs.frameworks {
            report["frameworks_ready"] = serde_json::json!(frameworks_ready);
        }
        return report;
    }
    let report = serde_json::json!({
        "kind": "auto_prime",
        "active_pack": pack.id,
        "pack_root": pack.root,
        "mcp_enabled": mcp_enabled,
        "preferred_coding_model": preferred_model,
        "agents_ready": agents_ready,
        "frameworks_ready": frameworks_ready,
    });
    let path = susi_paths::SusiDirs::config_dir().join("last_auto_prime.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()),
    );
    report
}

/// Auto-enable every ready leading MCP. `substrate` (never the cwd) is the
/// workspace its `{workspace}` args expand to.
fn prime_leading_mcp(substrate: &Path) -> Vec<String> {
    match susi_tools::LeadingMcpManager::new(substrate) {
        Ok(mcp) => match mcp.auto_enable_ready() {
            Ok(ids) => {
                if !ids.is_empty() && std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Auto-enabled MCP: {:?}", ids);
                }
                ids
            }
            Err(e) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] MCP auto-enable skipped: {e}");
                }
                Vec::new()
            }
        },
        Err(e) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] Leading MCP manager unavailable: {e}");
            }
            Vec::new()
        }
    }
}

/// Prefer the best ready coding model when none, or a no-longer-ready one,
/// is preferred.
fn prime_coding_models() -> Option<String> {
    match susi_gemi::models::coding_models::CodingModelManager::new() {
        Ok(models) => match models.auto_prefer_best_ready() {
            Ok(Some(id)) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Preferred coding model: {id}");
                }
                Some(id)
            }
            Ok(None) => None,
            Err(e) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Coding-model auto-prefer skipped: {e}");
                }
                None
            }
        },
        Err(e) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] CodingModelManager unavailable: {e}");
            }
            None
        }
    }
}

/// Scan one agent catalog: persist its `ready.json` and register a
/// capability for each ready entry. Returns how many entries were ready.
fn prime_agent_kind(substrate: &Path, kind: susi_agents::external::CatalogKind) -> usize {
    match susi_agents::external::AgentManager::for_kind(substrate, kind) {
        Ok(mgr) => match mgr.auto_prime_ready() {
            Ok(ready) => {
                if !ready.is_empty() && std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Auto-ready {}: {}", kind.label(), ready.len());
                }
                ready.len()
            }
            Err(e) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] {} auto-prime skipped: {e}", kind.label());
                }
                0
            }
        },
        Err(e) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] {} manager unavailable: {e}", kind.label());
            }
            0
        }
    }
}

#[cfg(test)]
mod catalog_selection_tests {
    use super::*;

    #[test]
    fn named_sets_select_exactly_their_catalogs() {
        assert!(Catalogs::NONE.is_empty());
        assert!(Catalogs::NONE.phases().is_empty());
        assert_eq!(Catalogs::default(), Catalogs::NONE);
        assert!(Catalogs::ALL.is_all());
        assert_eq!(
            Catalogs::ALL.phases(),
            vec![
                PRIME_PHASE_LEADING_MCP,
                PRIME_PHASE_CODING_MODELS,
                PRIME_PHASE_EXECUTION_AGENTS,
                PRIME_PHASE_FRAMEWORKS,
            ]
        );
        assert_eq!(
            Catalogs::LEADING_MCP.phases(),
            vec![PRIME_PHASE_LEADING_MCP]
        );
        assert_eq!(
            Catalogs::CODING_MODELS.phases(),
            vec![PRIME_PHASE_CODING_MODELS]
        );
        assert_eq!(
            Catalogs::AGENTS.phases(),
            vec![PRIME_PHASE_EXECUTION_AGENTS, PRIME_PHASE_FRAMEWORKS]
        );
    }

    #[test]
    fn all_is_the_union_of_every_catalog() {
        let all = Catalogs::LEADING_MCP
            .union(Catalogs::CODING_MODELS)
            .union(Catalogs::AGENTS);
        assert_eq!(all, Catalogs::ALL);
        assert!(!Catalogs::AGENTS.union(Catalogs::CODING_MODELS).is_all());
        assert_eq!(Catalogs::NONE.union(Catalogs::NONE), Catalogs::NONE);
    }
}

/// If a well-known local engine binary is on PATH but its API port is closed,
/// start the daemon once (best-effort; never blocks bootstrap on failure).
fn try_awaken_local_engines() {
    static AWOKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if AWOKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    if cfg!(test) {
        return;
    }
    // Ollama: `ollama serve` when binary present and :11434 is closed.
    if port_closed("127.0.0.1:11434") && binary_on_path("ollama") {
        let spawned = Command::new("ollama")
            .arg("serve")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        // The daemon parents this child for its whole life: reap it when it
        // exits, or it lingers as a zombie. A supervised one-shot, like
        // every other background helper.
        if let Ok(child) = spawned {
            let mut child = Some(child);
            crate::service_supervision::Supervisor::global().spawn_oneshot(
                "reap:ollama",
                0,
                move || {
                    child.take().map(|mut child| {
                        move || {
                            let _ = child.wait();
                        }
                    })
                },
            );
        }
        if std::env::var("SUSI_VERBOSE").is_ok() {
            eprintln!("[BOOTSTRAP] Awakened local engine: ollama serve");
        }
        // Brief settle so the subsequent /models probe can succeed this pass.
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn port_closed(addr: &str) -> bool {
    let Ok(sock) = addr.parse() else {
        return true;
    };
    TcpStream::connect_timeout(&sock, Duration::from_millis(150)).is_err()
}

fn binary_on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if candidate
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
                {
                    return true;
                }
            }
            #[cfg(not(unix))]
            {
                return true;
            }
        }
    }
    false
}

async fn prune_unhealthy_providers(registry: &CapabilityRegistry) {
    // Probe concurrently with a deadline so an unresponsive capability cannot
    // prevent automatic discovery and recovery of the rest of the substrate.
    let mut probes = tokio::task::JoinSet::new();
    for name in registry.list_providers() {
        if name == "Candle (Local)" {
            continue;
        }
        let Some(provider) = registry.get_provider(&name) else {
            continue;
        };
        probes.spawn(async move {
            let healthy = tokio::time::timeout(Duration::from_secs(5), provider.is_healthy())
                .await
                .is_ok_and(|result| result.unwrap_or(false));
            (name, healthy)
        });
    }
    while let Some(result) = probes.join_next().await {
        if let Ok((name, false)) = result {
            registry.unregister_provider(&name);
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] Pruned unhealthy provider: {}", name);
            }
        }
    }
}

/// Spawn a background loop that re-runs zero-config discovery so engines/tools
/// that appear after daemon start are hot-plugged without restart.
/// `interval_secs == 0` disables the loop (initial bootstrap still runs).
pub fn spawn_periodic_rediscovery(interval_secs: u64) {
    if interval_secs == 0 {
        return;
    }
    // Supervised: a full bootstrap pass probes engines and MCP servers
    // over the network and can legitimately take many minutes, so the
    // heartbeat bound is generous.
    crate::service_supervision::Supervisor::global().spawn(
        "capability-rediscovery",
        3,
        Some(Duration::from_secs(3600)),
        move || {
            Some(move || {
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("[SusiDaemon] Capability rediscovery loop aborted: {}", e);
                        return;
                    }
                };
                let interval = Duration::from_secs(interval_secs);
                loop {
                    std::thread::sleep(interval);
                    crate::service_supervision::heartbeat("capability-rediscovery");
                    runtime.block_on(bootstrap_zero_config_substrate());
                }
            })
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediscovery_disabled_when_interval_zero() {
        // Must not spawn / hang — just verify the early-return path.
        spawn_periodic_rediscovery(0);
    }

    // start_paused: tokio auto-advances the clock while idle, so the hung
    // provider's 5s probe deadline resolves instantly instead of in real
    // time — the hang-recovery assertion is unchanged.
    #[tokio::test(start_paused = true)]
    async fn prune_removes_unhealthy_non_candle_providers() {
        use susi_gemi::susi_core::provider::{BoxFuture, Provider};
        use susi_gemi::susi_core::susi_error::EaiResult;

        struct Unhealthy {
            hang: bool,
        }
        impl Provider for Unhealthy {
            fn name(&self) -> &str {
                if self.hang {
                    "ollama-hung"
                } else {
                    "ollama-dead"
                }
            }
            fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
                Box::pin(async move {
                    if self.hang {
                        std::future::pending::<()>().await;
                    }
                    Ok(false)
                })
            }
            fn generate(&self, _: &str) -> BoxFuture<'_, EaiResult<String>> {
                Box::pin(async { Ok(String::new()) })
            }
            fn embed(&self, _: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
                Box::pin(async { Ok(vec![]) })
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let registry = CapabilityRegistry::new();
        registry.register_provider(Unhealthy { hang: false });
        registry.register_provider(Unhealthy { hang: true });
        registry.register_provider(susi_gemi::engines::candle_provider::CandleProvider);
        tokio::time::timeout(Duration::from_secs(7), prune_unhealthy_providers(&registry))
            .await
            .expect("discovery must recover from a hanging provider");
        assert!(registry.get_provider("ollama-dead").is_none());
        assert!(registry.get_provider("ollama-hung").is_none());
        assert!(registry.get_provider("Candle (Local)").is_some());
    }
}
