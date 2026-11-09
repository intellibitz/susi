//! Host views and maintenance with no other plane: what `susi status
//! --json`, `susi review`, `susi os status|runtime|ecosystem`, `susi os
//! manage model uninstall`, `susi auto status|prime`, `susi admin
//! reload|provider-contract|intent-ladder` and `susi stop` do, and what the
//! matching MCP tools do over the `daemon.host` plane topic (zero-UI plan
//! B4 groups 2, 4, 9, 10 and 13). One implementation for both: the CLI
//! prints what these functions return.

use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

type R<T> = Result<T, String>;

fn msg(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The checkout containing `cwd` (`git rev-parse --show-toplevel`), else
/// `cwd` itself.
pub fn repo_root(cwd: &Path) -> PathBuf {
    susi_paths::console::background_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| cwd.to_path_buf())
}

// ── review (`susi review`) ─────────────────────────────────────────────────

/// `susi review`: daemon liveness and reclaimable OS cache, with the lines
/// the CLI prints.
pub fn review(workspace: &Path, global_dir: &Path) -> Value {
    use susi_gemi::models::hardware::HardwareProfiler;
    let os_report = HardwareProfiler::audit_os_environment_care();
    let pid = crate::SusiDaemon::check_status(workspace, global_dir);
    let mut lines = vec!["=== SUSI SUBSTRATE SUMMARY ===".to_string()];
    lines.push(match pid {
        Some(pid) => format!("- Global Daemon: Active (PID: {pid})"),
        None => "- Global Daemon: Inactive".to_string(),
    });
    lines.push(format!(
        "- Environment Care ({}) : Reclaimable {}",
        os_report.os_name, os_report.reclaimable_cache_formatted
    ));
    lines.push("\nQuick Action Commands:".to_string());
    lines.push("  susi os-clean  # Reclaim OS-level cache bloat".to_string());
    lines.push("  susi status    # System health pulse".to_string());
    json!({
        "daemon_pid": pid,
        "os_name": os_report.os_name,
        "reclaimable_cache": os_report.reclaimable_cache_formatted,
        "lines": lines,
    })
}

// ── auto (`susi auto status|prime`) ────────────────────────────────────────

/// `susi auto status`: zero-config substrate readiness.
#[derive(Debug, Serialize)]
pub struct AutoStatus {
    pub packs: Value,
    pub leading_mcp_enabled: Vec<String>,
    pub leading_mcp_ready_not_enabled: Vec<String>,
    pub preferred_coding_model: Option<String>,
    /// GGUF files present in the model directories (names only).
    pub local_models_installed: Vec<String>,
    /// `no local model installed` plus the download command when empty:
    /// core never downloads a model by itself (CEO decision 2026-11-04).
    pub local_model: String,
    pub agents_ready: usize,
    /// `None` (JSON null) when no framework readiness probe has run.
    pub frameworks_ready: Option<usize>,
    /// `probed`, or `not probed` until a framework readiness scan has
    /// recorded `agent-engines/ready.json`: core never probes framework
    /// adapters on its own (`susi auto prime` and `susi substrate status`
    /// record one on request; `susi frameworks doctor` reports live).
    pub frameworks_probe: &'static str,
    pub last_auto_prime: Option<Value>,
    pub last_blackboard: Option<Value>,
}

/// A bounded filename inventory, not model admission or recursive discovery.
/// Installed channels reuse existing model directories through these paths.
/// `susi status`, `susi auto status` and the first-run greeting report it:
/// reading names starts no probe and no download.
pub fn opening_model_inventory(config_home: &Path, data_home: &Path) -> Vec<String> {
    let mut roots = vec![data_home.join("models")];
    if let Some(dir) = std::env::var_os("SUSI_MODEL_DIR") {
        roots.push(PathBuf::from(dir));
    }
    if let Some(config) = std::fs::read_to_string(config_home.join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        roots.extend(
            config
                .get("local_scan_paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(16)
                .map(PathBuf::from),
        );
    }
    let mut models = std::collections::BTreeSet::new();
    for root in roots {
        for entry in std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .take(128)
            .flatten()
        {
            if entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
                && let Some(name) = entry.file_name().to_str()
            {
                models.insert(name.to_owned());
            }
            if models.len() >= 128 {
                break;
            }
        }
        if models.len() >= 128 {
            break;
        }
    }
    models.into_iter().collect()
}

fn load_json_file(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// `susi auto status` for `workspace` (its last mission blackboard).
pub fn auto_status(workspace: &Path) -> R<AutoStatus> {
    let packs = serde_json::to_value(susi_sandbox_client::extensions::list_packs()).map_err(msg)?;

    let mut enabled = Vec::new();
    let mut ready_not = Vec::new();
    if let Ok(mcp) = susi_tools::LeadingMcpManager::new(workspace)
        && let Ok(rows) = mcp.status()
    {
        for row in rows {
            let id = row
                .get("mcp")
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_enabled = row
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let ready = row
                .get("prerequisites_present")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_enabled {
                enabled.push(id);
            } else if ready {
                ready_not.push(id);
            }
        }
    }

    let preferred = susi_gemi::models::coding_models::CodingModelManager::new()
        .ok()
        .and_then(|m| m.preferred());

    let config = susi_paths::SusiDirs::config_dir();
    let local_models_installed =
        opening_model_inventory(&config, &susi_paths::SusiDirs::data_dir());
    let local_model = if local_models_installed.is_empty() {
        format!(
            "no local model installed in Susi's model folders (local runners such as \
             Ollama are still used when they are running); {}",
            susi_gemi::models::NoLocalModelInstalled::starter().install_hint()
        )
    } else {
        format!(
            "{} model file(s) found, verified when a goal runs",
            local_models_installed.len()
        )
    };
    let agents_ready = load_json_file(&config.join("execution-agents").join("ready.json"))
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0);
    let frameworks_ready = load_json_file(&config.join("agent-engines").join("ready.json"))
        .and_then(|v| v.as_array().map(|a| a.len()));
    let frameworks_probe = if frameworks_ready.is_some() {
        "probed"
    } else {
        "not probed"
    };

    let last_auto_prime = load_json_file(&config.join("last_auto_prime.json"));
    let last_blackboard = load_json_file(&workspace.join(".susi").join("last_blackboard.json"));

    Ok(AutoStatus {
        packs,
        leading_mcp_enabled: enabled,
        leading_mcp_ready_not_enabled: ready_not,
        preferred_coding_model: preferred,
        local_models_installed,
        local_model,
        agents_ready,
        frameworks_ready,
        frameworks_probe,
        last_auto_prime,
        last_blackboard: last_blackboard.map(|b| {
            json!({
                "agent_count": b.get("agent_count").cloned().unwrap_or(json!(0)),
                "path": ".susi/last_blackboard.json",
            })
        }),
    })
}

/// `susi auto prime`: re-run auto-prime (models and installed peers; never
/// enables MCP) against the substrate home.
pub fn auto_prime() -> Value {
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);
    crate::discovery_pipeline::prime_catalogs(&substrate)
}

// ── admin (`susi admin reload|provider-contract|intent-ladder`) ────────────

/// `susi admin reload`: reload `config.json` and report what is in effect;
/// `lines` are what the CLI prints.
pub fn config_reload(global_dir: &Path) -> R<Value> {
    let reloaded = susi_sandbox_client::manager::SusiConfig::reload(global_dir).map_err(msg)?;
    // Ladder resolution lives in gemi-models and takes that crate's
    // vendored `SusiConfig` (same JSON on disk as the sandbox service).
    let ladder_cfg =
        susi_gemi::models::susi_sandbox::manager::SusiConfig::load(global_dir).unwrap_or_default();
    let ladder_steps = susi_gemi::models::hf_discovery::resolve_model_ladder(&ladder_cfg).len();
    let mcp_servers = reloaded.bootstrap_mcp_servers::<Vec<Value>>().len();
    let source = global_dir.join("config.json");
    let lines = vec![
        format!(
            "Dynamic configuration reloaded successfully from {}.",
            source.display()
        ),
        format!("- Engine: {}", reloaded.default_engine()),
        format!("- Model: {}", reloaded.default_model()),
        format!("- Model Ladder Steps: {ladder_steps}"),
        format!("- MCP Bootstrap Servers: {mcp_servers}"),
    ];
    Ok(json!({
        "source": source,
        "engine": reloaded.default_engine(),
        "model": reloaded.default_model(),
        "model_ladder_steps": ladder_steps,
        "mcp_bootstrap_servers": mcp_servers,
        "lines": lines,
    }))
}

/// `susi admin provider-contract`: the fixture checks and, when the operator
/// opted in (`SUSI_PROVIDER_PROBE=1`), the read-only credentialed claims.
pub fn provider_contract() -> Value {
    // The credentialed half stays opt-in (SUSI_PROVIDER_PROBE=1) and
    // read-only — it proves auth and the error envelope and records the
    // generation-dependent kinds as Unsupported rather than spending tokens
    // to verify them.
    let fixture = susi_gemi::provider_contract::fixture_checks("fixture", "*", "1");
    let opted_in = std::env::var("SUSI_PROVIDER_PROBE").as_deref() == Ok("1");
    let credentialed =
        susi_gemi::provider_contract::probe_credentialed_opt_in("*", "*", "1").unwrap_or_default();
    json!({
        "fixture": fixture,
        "credentialed": {
            "opted_in": opted_in,
            "claims": credentialed,
        },
    })
}

/// What `susi admin intent-ladder` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentLadder {
    /// Read the last scorecard (the bare command).
    Scorecard,
    /// Run every rung now.
    Run,
    /// Run only the start-up slice.
    Slice,
    /// Re-check one intent id; the document's `verdict` is `pass` or not.
    Verify(String),
    /// The host outbox of failing rungs.
    Gaps,
    /// Adopting an outbox gap as a repair task: refused, naming
    /// `susi-commercial admin intent-ladder --adopt-gap` (the task queue
    /// moved to Susi Commercial Apps).
    AdoptGap(String),
}

/// `susi admin intent-ladder` for `cwd`.
pub fn intent_ladder(action: &IntentLadder, cwd: &Path) -> R<Value> {
    use susi_gawd::intent_ladder as ladder;
    let home = susi_paths::SusiDirs::substrate_home();
    match action {
        // Adopting a gap authors a task-queue record; the queue moved to
        // Susi Commercial Apps' dev workflow with the rest of `susi tasks`.
        IntentLadder::AdoptGap(id) => Err(format!(
            "adopting an intent gap authors a task-queue record, which moved to Susi \
             Commercial Apps: run `susi-commercial admin intent-ladder --adopt-gap {id}` in \
             the checkout"
        )),
        IntentLadder::Gaps => {
            let outbox = ladder::read_gaps(&home);
            let pending: Vec<Value> = outbox
                .pending()
                .map(|g| {
                    json!({
                        "intent": g.intent,
                        "grade": g.grade,
                        "failures": g.failures,
                        "adopt": g.adopt_command(),
                    })
                })
                .collect();
            Ok(json!({
                "outbox": ladder::gaps_path(&home),
                "pending": pending,
                "gaps": outbox.gaps,
            }))
        }
        IntentLadder::Verify(id) => {
            let runner = ladder::ProcessRunner::this_binary()
                .map_err(|e| format!("intent ladder runner unavailable: {e}"))?;
            let entry = ladder::verify_intent(cwd, id, &runner)
                .map_err(|e| format!("intent verify failed: {e}"))?;
            Ok(json!(entry))
        }
        IntentLadder::Run | IntentLadder::Slice => {
            let card = if *action == IntentLadder::Slice {
                ladder::startup_slice(&home, cwd)
            } else {
                ladder::full_run(&home, cwd)
            }
            .map_err(|e| format!("intent ladder run failed: {e}"))?;
            Ok(json!({
                "ladder_revision": card.ladder_revision,
                "source": card.ladder_source,
                "entries": card.entries,
                "scorecard": ladder::scorecard_path(&home),
            }))
        }
        IntentLadder::Scorecard => match ladder::read_scorecard(&home) {
            Some(card) => Ok(json!(card)),
            None => Err(format!(
                "no scorecard at {} — `--run` the ladder or `--slice` the start-up slice",
                ladder::scorecard_path(&home).display()
            )),
        },
    }
}

// ── models (`susi os manage model uninstall`) ──────────────────────────────

/// `susi os manage model uninstall <file.gguf>`: remove one SUSI-managed
/// model file (and its paired tokenizer). Refused for a path, a file outside
/// managed storage, a model the daemon has loaded (`loaded_models` reads the
/// daemon's inference cache; `None` when the daemon is not running) or the
/// selected model.
pub fn model_uninstall(name: &str, loaded_models: Option<&dyn Fn() -> R<Value>>) -> R<Value> {
    let requested = Path::new(name);
    if requested.components().count() != 1
        || requested.extension().and_then(|ext| ext.to_str()) != Some("gguf")
    {
        return Err("model uninstall accepts one managed .gguf filename, not a path".to_string());
    }
    let models_dir = susi_gemi::models::ModelManager::get_models_dir()
        .canonicalize()
        .map_err(|error| format!("resolve managed model directory: {error}"))?;
    let canonical = models_dir
        .join(requested)
        .canonicalize()
        .map_err(|error| format!("managed model `{name}` not found: {error}"))?;
    if canonical.parent() != Some(models_dir.as_path()) || !canonical.is_file() {
        return Err("refusing to remove a model outside SUSI-managed storage".to_string());
    }
    if let Some(loaded_models) = loaded_models {
        let loaded = loaded_models().map_err(|error| {
            format!("cannot confirm `{name}` is not loaded by the daemon: {error}")
        })?;
        let canonical_str = canonical.to_string_lossy();
        if loaded["models"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| m["path"].as_str() == Some(canonical_str.as_ref()))
        {
            return Err(format!(
                "`{name}` is loaded by the daemon; run `susi os manage model unload {name}` first"
            ));
        }
    }
    if susi_gemi::models::ModelManager::get_selected_model(None)
        .as_deref()
        .is_some_and(|selected| selected == name || selected == canonical.to_string_lossy())
    {
        return Err(format!(
            "`{name}` is selected; prefer another model before uninstalling it"
        ));
    }
    std::fs::remove_file(&canonical)
        .map_err(|error| format!("remove {}: {error}", canonical.display()))?;
    let stem = requested
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let tokenizer = models_dir.join(format!("{stem}.tokenizer.json"));
    let tokenizer_removed = match std::fs::remove_file(&tokenizer) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(format!(
                "model removed, but paired tokenizer {} could not be removed: {error}",
                tokenizer.display()
            ));
        }
    };
    Ok(json!({
        "uninstalled": canonical,
        "tokenizer_removed": tokenizer_removed,
        "message": format!(
            "uninstalled {}{}",
            canonical.display(),
            if tokenizer_removed {
                " and its paired tokenizer"
            } else {
                ""
            }
        ),
    }))
}

// ── daemon lifecycle (`susi stop`) ─────────────────────────────────────────

/// How long `daemon_shutdown` waits before stopping, so its reply reaches
/// the caller first.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// `susi stop` from inside the daemon: reply now, then stop this instance's
/// other daemon processes ([`crate::SusiDaemon::stop_instance_daemons`], the
/// call `susi stop` makes, which never signals its own process) and shut
/// this daemon down gracefully ([`crate::server::request_shutdown`], as on
/// SIGTERM). Starting a stopped daemon stays with the CLI the desktop runs.
pub fn shutdown_soon(global_dir: PathBuf) -> Value {
    let in_process = crate::server::daemon_in_process();
    // A one-shot worker on the supervision registry (VC-202-010).
    let mut job = Some(move || {
        std::thread::sleep(SHUTDOWN_GRACE);
        let _ = crate::SusiDaemon::stop_instance_daemons(&global_dir);
        crate::server::request_shutdown();
    });
    crate::service_supervision::Supervisor::global().spawn_oneshot(
        "socket-shutdown",
        0,
        move || job.take(),
    );
    json!({
        "stopping": true,
        "daemon_in_process": in_process,
        "grace_ms": SHUTDOWN_GRACE.as_millis() as u64,
        "note": "this instance's daemons stop after the reply; start it again with `susi start`",
    })
}

// ── socket dispatch ────────────────────────────────────────────────────────

fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The daemon's inference cache, read in process.
fn loaded_in_process() -> R<Value> {
    Ok(susi_core::plane_bus::gemi::loaded_models())
}

/// Dispatch one `daemon.host` request: `{action, workspace?, ..}`.
pub fn dispatch(payload: &Value) -> R<Value> {
    let workspace = field(payload, "workspace")
        .map(PathBuf::from)
        .unwrap_or_else(susi_paths::SusiDirs::substrate_home);
    let global_dir = susi_paths::SusiDirs::config_dir();
    match field(payload, "action").ok_or("action is required")? {
        "status" if payload.get("detail").and_then(Value::as_bool) == Some(true) => Ok(
            crate::status_report::to_json(&crate::status_report::collect(
                &workspace,
                &global_dir,
                &susi_paths::SusiDirs::substrate_home(),
                crate::status_report::now_unix(),
            )),
        ),
        "status" => crate::status_report::measured_json(&workspace, &global_dir).map_err(msg),
        "review" => Ok(review(&workspace, &global_dir)),
        "os_status" => Ok(crate::os_status::report()),
        "os_runtime" => Ok(crate::runtime_view::view(loaded_in_process)),
        "os_ecosystem" => crate::ecosystem_view::report(&workspace, loaded_in_process),
        "auto_status" => serde_json::to_value(auto_status(&workspace)?).map_err(msg),
        "auto_prime" => Ok(auto_prime()),
        "config_reload" => config_reload(&global_dir),
        "provider_contract" => Ok(provider_contract()),
        "intent_ladder" => {
            let id = || {
                field(payload, "id")
                    .map(str::to_string)
                    .ok_or_else(|| "id is required".to_string())
            };
            let action = match field(payload, "mode").unwrap_or("scorecard") {
                "scorecard" => IntentLadder::Scorecard,
                "run" => IntentLadder::Run,
                "slice" => IntentLadder::Slice,
                "gaps" => IntentLadder::Gaps,
                "verify" => IntentLadder::Verify(id()?),
                "adopt_gap" => IntentLadder::AdoptGap(id()?),
                other => {
                    return Err(format!(
                        "unknown intent-ladder mode `{other}` (expected scorecard, run, slice, gaps, verify or adopt_gap)"
                    ));
                }
            };
            intent_ladder(&action, &workspace)
        }
        "model_uninstall" => {
            let loaded: &dyn Fn() -> R<Value> = &loaded_in_process;
            model_uninstall(
                field(payload, "name").ok_or("name is required")?,
                Some(loaded),
            )
        }
        "daemon_shutdown" => Ok(shutdown_soon(global_dir)),
        other => Err(format!("unknown host action `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_validates_actions_and_arguments() {
        let refused = |payload: Value| dispatch(&payload).unwrap_err();
        assert!(refused(json!({})).contains("action"));
        assert!(refused(json!({"action": "explode"})).contains("unknown host action"));
        assert!(refused(json!({"action": "model_uninstall"})).contains("name"));
        assert!(
            refused(json!({"action": "model_uninstall", "name": "../escape.gguf"}))
                .contains("not a path")
        );
        assert!(
            refused(json!({"action": "intent_ladder", "mode": "verify"}))
                .contains("id is required")
        );
        assert!(
            refused(json!({"action": "intent_ladder", "mode": "nope"}))
                .contains("unknown intent-ladder")
        );
    }

    #[test]
    fn provider_contract_and_review_report_without_side_effects() {
        let contract = provider_contract();
        assert!(contract["credentialed"]["opted_in"].is_boolean());
        let ws = std::env::temp_dir();
        let doc = review(&ws, &ws);
        assert_eq!(doc["lines"][0], "=== SUSI SUBSTRATE SUMMARY ===");
        assert!(doc["reclaimable_cache"].is_string());
    }

    #[test]
    fn detailed_status_carries_every_section_the_human_view_prints() {
        use crate::status_report::*;
        let status = UnifiedStatus {
            workspace: PathBuf::from("/ws"),
            daemon_pid: Some(42),
            missions: vec![MissionInFlight {
                id: "m1".into(),
                prompt_head: "fix the build".into(),
                dispatched_unix: 7,
            }],
            resumable: vec![ResumableMission {
                id: "dag1".into(),
                resumable_nodes: vec!["n2".into()],
                interruptions: 1,
            }],
            rollback_ready: vec![RollbackReady {
                id: "f1".into(),
                snapshots: 2,
            }],
            queue: QueueSection::default(),
            brain: BrainSection {
                leaders: vec![("code".into(), "p1".into())],
                unfit: vec!["p2|code".into()],
                ..BrainSection::default()
            },
            routing: RoutingSection {
                preferred_cloud: Some("c1".into()),
                cooled: vec![("p3".into(), 30)],
                ..RoutingSection::default()
            },
            spend: SpendSection {
                calls: 3,
                by_provider: vec![("p1".into(), 3, 2)],
                ..SpendSection::default()
            },
            approvals: vec![PendingApproval {
                id: "a1".into(),
                requester: "r".into(),
                resource: "res".into(),
                action: "act".into(),
            }],
        };
        let doc = to_json(&status);
        let text = render(&status);
        assert_eq!(doc["missions_in_flight"][0]["prompt_head"], "fix the build");
        assert!(text.contains("fix the build"));
        assert_eq!(
            doc["resumable_missions"][0]["resumable_nodes"],
            json!(["n2"])
        );
        assert_eq!(doc["rollback_ready"][0]["snapshots"], 2);
        assert_eq!(doc["brain"]["leaders"][0]["provider"], "p1");
        assert_eq!(doc["brain"]["unfit"], json!(["p2|code"]));
        assert_eq!(doc["routing"]["preferred_cloud"], "c1");
        assert_eq!(doc["routing"]["cooling_down"][0]["remaining_secs"], 30);
        assert_eq!(doc["spend"]["by_provider"][0]["successes"], 2);
        assert_eq!(doc["pending_approvals"][0]["requester"], "r");
        assert_eq!(doc["daemon"]["pid"], 42);
    }

    #[test]
    fn repo_root_falls_back_to_the_directory() {
        let dir = std::env::temp_dir().join(format!("susi-host-views-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = repo_root(&dir);
        assert!(root == dir || dir.starts_with(&root));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_uninstall_rejects_non_gguf_extension_and_empty_name() {
        assert!(
            model_uninstall("weights.bin", None)
                .unwrap_err()
                .contains("not a path")
        );
        assert!(
            model_uninstall("", None)
                .unwrap_err()
                .contains("not a path")
        );
    }
}
