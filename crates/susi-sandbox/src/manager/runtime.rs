//! Sandbox runtime helpers: docker exec, audit, backup, intent bundles, memory.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

use susi_config::confined_workspace_join;
use susi_config::SusiConfig;
use susi_config::*;

// === SANDBOX MANAGER ===
pub struct SandboxManager;

impl SandboxManager {
    pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
        use bollard::container::LogOutput;
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::{
            CreateContainerOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
        };
        use bollard::Docker;
        use futures::stream::StreamExt;

        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| EaiError::process(format!("Docker connection failed: {}", e)))?;

        let sandbox_image = SusiConfig::load_global()
            .unwrap_or_default()
            .sandbox_image();
        // Hardened defaults: no network, drop capabilities, read-only rootfs,
        // memory cap, auto-remove. Authenticated callers still get a shell
        // inside the image — isolation limits blast radius, not intent.
        let config = ContainerCreateBody {
            image: Some(sandbox_image),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), cmd.to_string()]),
            network_disabled: Some(true),
            host_config: Some(HostConfig {
                network_mode: Some("none".to_string()),
                readonly_rootfs: Some(true),
                cap_drop: Some(vec!["ALL".to_string()]),
                memory: Some(256 * 1024 * 1024),
                nano_cpus: Some(500_000_000), // 0.5 CPU
                auto_remove: Some(true),
                security_opt: Some(vec!["no-new-privileges:true".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let container = docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .map_err(|e| EaiError::process(format!("Container creation failed: {}", e)))?;

        docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| EaiError::process(format!("Container start failed: {}", e)))?;

        let mut logs = docker.logs(
            &container.id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                follow: true,
                ..Default::default()
            }),
        );
        let mut output = String::new();
        while let Some(log) = logs.next().await {
            match log {
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                    output.push_str(&String::from_utf8_lossy(&message));
                }
                Ok(_) => {}
                Err(e) => {
                    output.push_str(&format!("\n[docker log error: {e}]"));
                    break;
                }
            }
        }

        // Best-effort cleanup if auto_remove did not fire (e.g. never started).
        let _ = docker
            .remove_container(
                &container.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        Ok(output)
    }

    pub fn ensure_gitignore_purity(workspace: &Path) {
        let gitignore = workspace.join(".gitignore");
        if gitignore.exists() {
            if let Ok(content) = fs::read_to_string(&gitignore) {
                if !content.contains(".susi") {
                    if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&gitignore) {
                        use std::io::Write;
                        let _ = writeln!(f, "\n# SUSI Substrate ephemeral state\n.susi/");
                    }
                }
            }
        }
    }

    pub fn ensure_global_sandbox(global_dir: &Path) -> EaiResult<()> {
        Self::ensure_gitignore_purity(global_dir);
        if !global_dir.exists() {
            fs::create_dir_all(global_dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        let config_path = SusiConfig::get_config_path(global_dir);
        if !config_path.exists() {
            let default_cfg_file = Path::new("config.default.json");
            let cfg = if default_cfg_file.is_file() {
                fs::read_to_string(default_cfg_file)
                    .ok()
                    .and_then(|c| serde_json::from_str::<SusiConfig>(&c).ok())
                    .unwrap_or_default()
            } else {
                SusiConfig::default()
            };
            let json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".to_string());
            fs::write(config_path, json).map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        Ok(())
    }

    pub fn save_mission_checkpoint(workspace: &Path, checkpoint: &NeuralCheckpoint) {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let _ = fs::write(
            susi_dir.join("mission_checkpoint.json"),
            serde_json::to_string_pretty(checkpoint).unwrap_or_default(),
        );
    }

    pub fn check_interrupted_checkpoint(workspace: &Path) -> Option<NeuralCheckpoint> {
        let p = workspace.join(".susi/mission_checkpoint.json");
        if p.is_file() {
            fs::read_to_string(&p)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
        } else {
            None
        }
    }
}

// === AUDIT LOGGER & LOG LEVEL ===
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
    Axiomatic,
    Debug,
    Trace,
}

pub struct SusiAuditLogger;

impl SusiAuditLogger {
    pub fn log_event(workspace: &Path, event_type: &str, details: &str) {
        Self::log(workspace, LogLevel::Info, event_type, details);
    }

    pub fn log(workspace: &Path, level: LogLevel, event_type: &str, details: &str) {
        let susi_dir = workspace.join(".susi");
        if !susi_dir.exists() {
            let _ = fs::create_dir_all(&susi_dir);
        }
        let audit_file = workspace.join(".susi/audit.log");

        // Deterministic credential masking (Mandate 10: No Secret Leaks) — every
        // telemetry write funnels through here, so this is the one chokepoint
        // that guarantees secrets never reach the persistent audit trail. Loads
        // config and redacts locally (rather than calling into
        // `gawd::security::SecurityDetector::redact`) so `sandbox` doesn't
        // depend on `gawd` just to reach a pure text-transform primitive.
        let global_dir = susi_paths::SusiDirs::config_dir();
        let secret_patterns = SusiConfig::load(&global_dir)
            .map(|cfg| cfg.governance().secret_tokens)
            .unwrap_or_default();
        let details = susi_error::redact::redact_patterns(&secret_patterns, details);
        let details = details.as_str();

        tracing::info!(
            target: "susi_audit",
            event_type = event_type,
            level = ?level,
            workspace = %workspace.display(),
            details = details,
            "audit_event"
        );

        // Cryptographic accountability chain: hash-linked + HMAC-SHA256 under
        // ~/.susi/audit.hmac.key (immutable without the host key).
        if let Err(e) = crate::audit_chain::append_signed_entry(
            &audit_file,
            &format!("{:?}", level),
            event_type,
            details,
            std::process::id(),
        ) {
            tracing::warn!(
                target: "susi_audit",
                "failed to append signed audit entry: {}",
                e
            );
        }
    }

    pub fn read_audit_log(workspace: &Path, limit: usize) -> String {
        let audit_file = workspace.join(".susi/audit.log");
        if let Ok(content) = fs::read_to_string(audit_file) {
            let lines: Vec<&str> = content.lines().collect();
            let start = if lines.len() > limit {
                lines.len() - limit
            } else {
                0
            };
            return lines[start..].join("\n");
        }
        String::new()
    }
}

// === BACKUP MANAGER ===
pub struct SusiBackupManager;

impl SusiBackupManager {
    pub fn backup_work(workspace: &Path) -> EaiResult<String> {
        let backups_dir = workspace.join(".susi/backups");
        let _ = fs::create_dir_all(&backups_dir);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup_path = backups_dir.join(format!("backup_{}", ts));

        Self::recursive_copy(workspace, &backup_path, &backups_dir)?;

        Ok(format!("Backup created at {}", backup_path.display()))
    }

    fn recursive_copy(src: &Path, dst: &Path, exclude: &Path) -> EaiResult<()> {
        if src == exclude {
            return Ok(());
        }

        if src.is_dir() {
            fs::create_dir_all(dst)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                let path = entry.path();
                let dest_path = dst.join(entry.file_name());
                Self::recursive_copy(&path, &dest_path, exclude)?;
            }
        } else {
            fs::copy(src, dst)?;
        }
        Ok(())
    }
}

// === Intent Bundle Manager - Now 100% dynamic ===
pub struct IntentBundleManager;

impl IntentBundleManager {
    fn bundles_path(workspace: &Path) -> PathBuf {
        workspace.join(".susi/staged_bundles.json")
    }

    pub fn get_staged_bundles(workspace: &Path) -> Vec<IntentBundle> {
        let p = Self::bundles_path(workspace);
        if p.is_file() {
            fs::read_to_string(&p)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_staged_bundles(workspace: &Path, bundles: &[IntentBundle]) -> EaiResult<()> {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let json = serde_json::to_string_pretty(bundles)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        fs::write(Self::bundles_path(workspace), json)
            .map_err(|e| EaiError::filesystem(e.to_string()))
    }

    pub fn stage_bundle(workspace: &Path, bundle: IntentBundle) -> EaiResult<()> {
        let mut bundles = Self::get_staged_bundles(workspace);
        bundles.retain(|b| b.bundle_id() != bundle.bundle_id());
        bundles.push(bundle);
        Self::save_staged_bundles(workspace, &bundles)
    }

    pub fn accept_all(workspace: &Path) -> EaiResult<String> {
        let mut bundles = Self::get_staged_bundles(workspace);
        if bundles.is_empty() {
            return Ok("No staged intent bundles to accept.".to_string());
        }
        let mut accepted_count = 0;
        let mut files_changed = 0;
        for bundle in &mut bundles {
            if !bundle.is_applied() {
                for fix in &bundle.staged_fixes {
                    let target_path = confined_workspace_join(workspace, fix.file_path())?;
                    if let Some(parent) = target_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::write(&target_path, fix.staged_content());
                    files_changed += 1;
                }
                bundle.set_applied(true);
                accepted_count += 1;
            }
        }
        Self::save_staged_bundles(workspace, &bundles)?;
        Ok(format!(
            "SUCCESS: Accepted {} bundles across {} files.",
            accepted_count, files_changed
        ))
    }

    pub fn rollback_all(workspace: &Path) -> EaiResult<String> {
        let mut bundles = Self::get_staged_bundles(workspace);
        if bundles.is_empty() {
            return Ok("No staged intent bundles to rollback.".to_string());
        }

        let mut reverted_files = 0;
        for bundle in &mut bundles {
            if bundle.is_applied() {
                for fix in &bundle.staged_fixes {
                    let target_path = confined_workspace_join(workspace, fix.file_path())?;
                    if !fix.original_content().is_empty() {
                        let _ = fs::write(&target_path, fix.original_content());
                    } else if target_path.exists() {
                        let _ = fs::remove_file(&target_path);
                    }
                    reverted_files += 1;
                }
                bundle.set_applied(false);
            }
        }

        let _ = fs::remove_file(Self::bundles_path(workspace));
        Ok(format!(
            "SUCCESS: Rolled back staged fixes across {} files.",
            reverted_files
        ))
    }
}

pub struct SusiMemory;
impl SusiMemory {
    /// `engine_version` is the caller's `SUSI_VERSION` (the root `susi`
    /// package version) - `susi-sandbox` doesn't know it at compile time
    /// (its own crate version is unrelated), so callers pass it explicitly,
    /// same as the rest of the codebase already threads it through
    /// `solve_clean`/`solve_stream` call sites.
    pub fn save_interaction(workspace: &Path, input: &str, output: &str, engine_version: &str) {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let memory_file = susi_dir.join("memory.jsonl");

        if input.trim().is_empty() || output.trim().is_empty() {
            return;
        }

        let entry = serde_json::json!({
            "intent": input,
            "outcome": output,
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "provenance": {
                "workspace": workspace.display().to_string(),
                "engine_version": engine_version,
            }
        });

        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&memory_file)
        {
            let _ = writeln!(f, "{}", entry);
        }

        let heuristics = SusiConfig::load_global()
            .unwrap_or_default()
            .memory_experience_heuristics();
        let has_failure_marker = heuristics
            .failure_markers
            .iter()
            .any(|marker| output.contains(marker.as_str()));
        if output.len() > heuristics.min_output_len && !has_failure_marker {
            let exp_file = susi_dir.join("reasoning_experience.jsonl");
            let exp_entry = serde_json::json!({
                "intent": input,
                "blackboard_context": "converged",
                "successful_outcome": output,
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "validation": "STRICT_SEMANTIC_PASS"
            });
            if let Ok(mut f) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(exp_file)
            {
                let _ = writeln!(f, "{}", exp_entry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use susi_config::{merge_missing_registry_defaults, DynamicRegistry, DynamicValue};

    #[test]
    fn test_checkpoint_lifecycle() {
        let ws = Path::new(".");
        let mut fields = DynamicRegistry::new();
        fields.insert("intent".to_string(), serde_json::json!("test"));
        let cp = NeuralCheckpoint { fields };
        SandboxManager::save_mission_checkpoint(ws, &cp);
        let loaded = SandboxManager::check_interrupted_checkpoint(ws);
        assert!(loaded.is_some());
        let _ = fs::remove_dir_all(ws.join(".susi"));
    }

    #[test]
    fn test_susi_config_lifecycle() {
        let dir = Path::new("test_cfg");
        let _ = fs::create_dir_all(dir);
        let _ = SandboxManager::ensure_global_sandbox(dir);
        let cfg = SusiConfig::load(dir).expect("Failed to load config");
        assert_eq!(cfg.gmcp_port(), 9090);
        let _ = fs::remove_dir_all(dir);
    }

    /// Every scalar accessor's only fallback is config.default.json itself
    /// (via get_or_bundled_default) — there is no second, Rust-literal copy
    /// of any default that could drift out of sync with it. Host-contract
    /// ports are an exception: accessors always return `susi_paths::ports`
    /// (JSON port fields are documentation-only and are not heal-merged into
    /// host `config.json`).
    #[test]
    fn test_config_accessors_match_bundled_default_single_source_of_truth() {
        let default = SusiConfig::default();
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../../../config/config.default.json")).unwrap();

        // Host contract: accessors ignore JSON; bundled docs must still match constants.
        assert_eq!(default.gmcp_port(), susi_paths::ports::GMCP);
        assert_eq!(
            raw["gmcp_port"].as_u64().unwrap() as u16,
            susi_paths::ports::GMCP
        );
        assert_eq!(default.gmcp_http_port(), susi_paths::ports::GMCP_HTTP);
        assert_eq!(
            raw["gmcp_http_port"].as_u64().unwrap() as u16,
            susi_paths::ports::GMCP_HTTP
        );
        assert_eq!(default.gemi_port(), susi_paths::ports::GEMI);
        assert_eq!(
            raw["gemi_port"].as_u64().unwrap() as u16,
            susi_paths::ports::GEMI
        );
        assert_eq!(
            default.udp_discovery_port(),
            susi_paths::ports::UDP_DISCOVERY
        );
        assert_eq!(
            raw["udp_discovery_port"].as_u64().unwrap() as u16,
            susi_paths::ports::UDP_DISCOVERY
        );
        assert_eq!(default.trust_level(), raw["trust_level"].as_str().unwrap());
        assert_eq!(
            default.max_stdin_size_bytes(),
            raw["max_stdin_size_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.max_rpc_body_bytes(),
            raw["max_rpc_body_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.allow_origin(),
            raw["allow_origin"].as_str().unwrap()
        );
        assert_eq!(
            default.mcp_registry_url(),
            raw["mcp_registry_url"].as_str().unwrap()
        );
        assert_eq!(
            default.alpha_weights_url(),
            raw["alpha_weights_url"].as_str().unwrap()
        );
        assert_eq!(
            default.agent_rank_threshold(),
            raw["agent_rank_threshold"].as_f64().unwrap() as f32
        );
        assert_eq!(
            default.cloud_scout_timeout_secs(),
            raw["cloud_scout_timeout_secs"].as_u64().unwrap()
        );
        assert_eq!(
            default.model_provisioning_wait_secs(),
            raw["model_provisioning_wait_secs"].as_u64().unwrap()
        );
        assert_eq!(
            default.reflex_training_threshold(),
            raw["reflex_training_threshold"].as_u64().unwrap() as usize
        );
        assert_eq!(default.qdrant_url(), raw["qdrant_url"].as_str().unwrap());
        assert_eq!(
            default.crates_io_api_url(),
            raw["crates_io_api_url"].as_str().unwrap()
        );
        assert_eq!(
            default.sandbox_image(),
            raw["sandbox_image"].as_str().unwrap()
        );

        let heuristics = default.memory_experience_heuristics();
        assert_eq!(
            heuristics.min_output_len,
            raw["memory_experience_heuristics"]["min_output_len"]
                .as_u64()
                .unwrap() as usize
        );
        assert!(!heuristics.failure_markers.is_empty());

        assert_eq!(
            default.max_generation_tokens(),
            raw["max_generation_tokens"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.repeat_penalty(),
            raw["repeat_penalty"].as_f64().unwrap() as f32
        );
        assert_eq!(
            default.repeat_last_n(),
            raw["repeat_last_n"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.eos_token_ids(),
            raw["eos_token_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u32)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            default.axiomatic_risk_patterns().len(),
            raw["axiomatic_risk_patterns"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_scan_exclude_dirs().len(),
            raw["model_scan_exclude_dirs"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_discovery_exclude_dirs().len(),
            raw["model_discovery_exclude_dirs"]
                .as_array()
                .unwrap()
                .len()
        );
        assert_eq!(
            default.home_scan_root_exclude_dirs().len(),
            raw["home_scan_root_exclude_dirs"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_file_extensions().len(),
            raw["model_file_extensions"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_file_min_bytes(),
            raw["model_file_min_bytes"].as_u64().unwrap()
        );

        // model_discovery_exclude_dirs (used by deep_scan_home_and_register,
        // which scans *inside* .cache/.local/.android as seeded roots) must
        // never exclude those two dir names, unlike the broader
        // model_scan_exclude_dirs — this is the exact invariant that keeps
        // JetBrains/ProxyAI model discovery under ~/.cache working.
        assert!(!default
            .model_discovery_exclude_dirs()
            .iter()
            .any(|d| d == ".cache" || d == "Library"));
    }

    #[test]
    fn test_susi_config_hot_reload_lifecycle() {
        let dir = Path::new("test_hot_reload_cfg");
        let _ = fs::create_dir_all(dir);
        let _ = SandboxManager::ensure_global_sandbox(dir);

        let mut cfg = SusiConfig::load(dir).expect("Failed to load initial config");
        assert_eq!(cfg.execution_lease_secs(), 30);
        assert_eq!(cfg.max_concurrent_agents(), 32);

        // Modify config externally and verify dynamic reload
        cfg.settings
            .insert("execution_lease_secs".to_string(), serde_json::json!(45));
        cfg.settings
            .insert("max_concurrent_agents".to_string(), serde_json::json!(64));
        cfg.save(dir).expect("Failed to save updated config");

        let reloaded = SusiConfig::reload(dir).expect("Failed to reload config");
        assert_eq!(reloaded.execution_lease_secs(), 45);
        assert_eq!(reloaded.max_concurrent_agents(), 64);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_susi_config_backfills_schema_drift_without_clobbering_user_values() {
        let dir = Path::new("test_cfg_migration");
        let _ = fs::create_dir_all(dir);

        // Simulate a pre-existing user config.json that predates a new field
        // (tokenizer_repo) added to one ladder step in config.default.json,
        // and that has customized an unrelated existing value.
        let stale = serde_json::json!({
            "trust_level": "paranoid",
            "gmcp_port": 12345,
            "model_ladder": [
                {
                    "hf_file": "qwen2.5-1.5b-instruct-q4_k_m.gguf",
                    "hf_repo": "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
                    "label": "1.5B Parameters (Fast Local Edge)",
                    "min_ram_gb": 0,
                    "step": 1
                }
            ]
        });
        fs::write(
            SusiConfig::get_config_path(dir),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();

        let cfg = SusiConfig::load(dir).expect("Failed to load stale config");

        // Public ports are a hard contract — polluted values cannot override them.
        assert_eq!(cfg.gmcp_port(), susi_paths::ports::GMCP);
        // User's customized non-port scalar survives the merge untouched.
        assert_eq!(cfg.trust_level(), "paranoid");

        // An explicit user ladder is preserved; the default is now dynamic.
        let custom_ladder = cfg.model_ladder();
        assert_eq!(custom_ladder.len(), 1);
        assert_eq!(custom_ladder[0].hf_repo, "Qwen/Qwen2.5-1.5B-Instruct-GGUF");
        let fallback = cfg.default_fallback_model();
        assert!(!fallback.tokenizer_repo.is_empty());

        // Missing nested lifecycle settings heal without replacing user tuning.
        let mut settings = SusiConfig::default().settings;
        if let Some(serde_json::Value::Object(policy)) = settings.get_mut("model_lifecycle") {
            policy.remove("download_attempts");
            policy.insert("max_parallel_downloads".into(), serde_json::json!(1));
        }
        SusiConfig { settings }.save(dir).unwrap();
        let reloaded = SusiConfig::load(dir).expect("Failed to load stale lifecycle config");
        let policy = reloaded.model_lifecycle();
        assert_eq!(
            policy.download_attempts,
            SusiConfig::default().model_lifecycle().download_attempts
        );
        assert_eq!(policy.max_parallel_downloads, 1);
        assert_eq!(
            reloaded.settings.get("model_ladder"),
            Some(&serde_json::json!([]))
        );

        // The merge must have persisted back to disk (self-healing).
        let on_disk = fs::read_to_string(SusiConfig::get_config_path(dir)).unwrap();
        assert!(on_disk.contains("download_attempts"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_susi_memory_lifecycle() {
        let ws = Path::new("test_mem");
        let _ = fs::create_dir_all(ws);
        SusiMemory::save_interaction(ws, "hello", "world", "test");
        let memory_file = ws.join(".susi/memory.jsonl");
        assert!(memory_file.is_file());
        let _ = fs::remove_dir_all(ws);
    }

    /// The experience-promotion threshold (min_output_len, failure_markers)
    /// is config-driven now, not a hardcoded `output.len() > 50` literal —
    /// prove the configured values actually gate behavior, not just that the
    /// accessor returns the right number.
    #[test]
    fn test_susi_memory_experience_promotion_respects_config_heuristics() {
        let ws = Path::new("test_mem_experience");
        let _ = fs::create_dir_all(ws);
        let exp_file = ws.join(".susi/reasoning_experience.jsonl");

        let heuristics = SusiConfig::default().memory_experience_heuristics();
        let short_output = "x".repeat(heuristics.min_output_len); // exactly at threshold: not > min_output_len
        SusiMemory::save_interaction(ws, "goal a", &short_output, "test");
        assert!(
            !exp_file.exists(),
            "output at, not over, the threshold must not be promoted"
        );

        let long_output = "x".repeat(heuristics.min_output_len + 1);
        SusiMemory::save_interaction(ws, "goal b", &long_output, "test");
        assert!(
            exp_file.is_file(),
            "output over the threshold must be promoted"
        );

        let with_marker = format!("{} {}", heuristics.failure_markers[0], long_output);
        let before = fs::read_to_string(&exp_file).unwrap();
        SusiMemory::save_interaction(ws, "goal c", &with_marker, "test");
        let after = fs::read_to_string(&exp_file).unwrap();
        assert_eq!(
            before, after,
            "a configured failure marker must suppress promotion"
        );

        let _ = fs::remove_dir_all(ws);
    }

    #[test]
    fn test_intent_bundle_staging_and_rollback_lifecycle() {
        let ws = Path::new("test_bundle_ws");
        let _ = fs::create_dir_all(ws);

        let test_file = ws.join("test_code.txt");
        let _ = fs::write(&test_file, "original code");

        let mut fix_fields = DynamicRegistry::new();
        fix_fields.insert("file_path".to_string(), serde_json::json!("test_code.txt"));
        fix_fields.insert(
            "original_content".to_string(),
            serde_json::json!("original code"),
        );
        fix_fields.insert(
            "staged_content".to_string(),
            serde_json::json!("refactored code"),
        );

        let mut bundle_fields = DynamicRegistry::new();
        bundle_fields.insert("bundle_id".to_string(), serde_json::json!("b1"));

        let bundle = IntentBundle {
            fields: bundle_fields,
            staged_fixes: vec![StagedFix { fields: fix_fields }],
            applied: false,
            title: "Test Bundle".to_string(),
        };

        IntentBundleManager::stage_bundle(ws, bundle).expect("Staging failed");
        let staged = IntentBundleManager::get_staged_bundles(ws);
        assert_eq!(staged.len(), 1);

        IntentBundleManager::accept_all(ws).expect("Accept failed");
        let content = fs::read_to_string(&test_file).unwrap_or_default();
        assert_eq!(content, "refactored code");

        IntentBundleManager::rollback_all(ws).expect("Rollback failed");
        let rolled_back = fs::read_to_string(&test_file).unwrap_or_default();
        assert_eq!(rolled_back, "original code");

        let _ = fs::remove_dir_all(ws);
    }

    #[test]
    fn test_backfill_missing_prompt_keys_adds_new_key_without_touching_existing() {
        // Regression: a prompts.json written before dynamic_agent_prompt (or
        // any future key) existed would otherwise permanently lack it, since
        // load_global() only writes prompts.json once, on first-ever load.
        let mut existing: DynamicRegistry = HashMap::new();
        existing.insert(
            "consensus_wisdom_prompt".to_string(),
            DynamicValue::String("old customized text".to_string()),
        );

        let mut defaults: DynamicRegistry = HashMap::new();
        defaults.insert(
            "consensus_wisdom_prompt".to_string(),
            DynamicValue::String("new default text".to_string()),
        );
        defaults.insert(
            "dynamic_agent_prompt".to_string(),
            DynamicValue::String("direct-answer prompt".to_string()),
        );

        let changed = merge_missing_registry_defaults(&mut existing, &defaults);

        assert!(changed);
        assert_eq!(
            existing
                .get("consensus_wisdom_prompt")
                .and_then(|v| v.as_str()),
            Some("old customized text"),
            "an existing key must never be overwritten by a newer default"
        );
        assert_eq!(
            existing
                .get("dynamic_agent_prompt")
                .and_then(|v| v.as_str()),
            Some("direct-answer prompt"),
            "a key missing from the user's file must be backfilled from the bundled default"
        );
    }

    #[test]
    fn test_backfill_missing_prompt_keys_reports_no_change_when_nothing_missing() {
        let mut existing: DynamicRegistry = HashMap::new();
        existing.insert(
            "consensus_wisdom_prompt".to_string(),
            DynamicValue::String("text".to_string()),
        );
        let defaults = existing.clone();

        assert!(!merge_missing_registry_defaults(&mut existing, &defaults));
    }

    #[test]
    fn test_susi_prompts_template_lifecycle() {
        let prompts = SusiPrompts::default();
        let formatted = prompts.format_chat_prompt("Qwen2.5-32B", "System Text", "User Text");
        assert!(formatted.contains("System Text"));
        assert!(formatted.contains("User Text"));

        let mut vars = HashMap::new();
        vars.insert("system".to_string(), "Sys".to_string());
        vars.insert("prompt".to_string(), "Usr".to_string());
        let fmt = prompts.format_dynamic("Unknown-Model", vars);
        assert!(fmt.contains("Sys"));
        assert!(fmt.contains("Usr"));
    }

    #[test]
    fn test_dynamic_chat_template_rendering() {
        let mut cfg = ChatTemplateConfig::default();
        cfg.templates.clear();
        cfg.templates.insert(
            "deepseek".to_string(),
            "### System:\n{system}\n\n### User:\n{prompt}\n\n### Assistant:\n".to_string(),
        );
        cfg.templates.insert(
            "mistral".to_string(),
            "[INST] {system} {prompt} [/INST]".to_string(),
        );
        cfg.templates.insert(
            "phi".to_string(),
            "<|system|>\n{system}<|end|>\n<|user|>\n{prompt}<|end|>\n<|assistant|>".to_string(),
        );

        let mut vars = HashMap::new();
        vars.insert("system".to_string(), "Sys".to_string());
        vars.insert("prompt".to_string(), "Usr".to_string());

        let mistral_fmt = cfg.render("Mistral-7B-Instruct", &vars);
        assert!(mistral_fmt.contains("[INST]"));

        let phi_fmt = cfg.render("Phi-3-Mini", &vars);
        assert!(phi_fmt.contains("<|user|>"));

        let deepseek_fmt = cfg.render("DeepSeek-R1-Distill", &vars);
        assert!(deepseek_fmt.contains("### Assistant:"));
    }

    #[test]
    fn model_catalog_is_curated_ladder() {
        let models = SusiConfig::default().model_catalog();
        assert!(
            (40..=60).contains(&models.len()),
            "expected ~40–60 curated models, got {}",
            models.len()
        );
        assert!(
            !models.iter().any(|m| m.id.contains("catalog/model-")),
            "filler catalog ids must be removed"
        );
    }

    #[test]
    fn leading_catalogs_meet_trustworthy_floors() {
        // external_peer_agents() -> load_json_or_bundled -> ensure_extensions_substrate()
        // performs a read-modify-write on the extensions state.json, whose path
        // derives from HOME; serialize against tests that swap HOME so it
        // cannot clobber their temp-root state.
        let _guard = crate::env_test_lock();
        let peers = SusiConfig::default().external_peer_agents();
        assert!(peers.len() >= 20, "agents {}", peers.len());
        let engines = SusiConfig::default().inference_endpoints().endpoints;
        assert!(
            (12..=24).contains(&engines.len()),
            "engines {}",
            engines.len()
        );
        let mcp: Vec<serde_json::Value> =
            serde_json::from_str(SusiConfig::leading_mcp_registry_json()).unwrap();
        assert!((50..=120).contains(&mcp.len()), "mcp {}", mcp.len());
    }
}
