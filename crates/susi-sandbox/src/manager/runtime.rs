//! Sandbox runtime helpers: docker exec, audit, memory.
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::susi_config::SusiConfig;
use crate::susi_config::*;

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
                // Fork bombs stay inside the container's pid budget.
                pids_limit: Some(128),
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
        // Bound wall time and captured output: a `sleep infinity` or an
        // endless printer must not hold the request (or daemon memory) open.
        const MAX_RUNTIME: std::time::Duration = std::time::Duration::from_secs(120);
        const MAX_OUTPUT: usize = 1024 * 1024;
        let mut output = String::new();
        let collect = async {
            while let Some(log) = logs.next().await {
                match log {
                    Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                        output.push_str(&String::from_utf8_lossy(&message));
                        if output.len() > MAX_OUTPUT {
                            output.truncate(MAX_OUTPUT);
                            output.push_str("\n[output truncated at 1 MiB]");
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        output.push_str(&format!("\n[docker log error: {e}]"));
                        break;
                    }
                }
            }
        };
        if tokio::time::timeout(MAX_RUNTIME, collect).await.is_err() {
            output.push_str("\n[sandbox command timed out after 120s; container killed]");
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

    pub fn ensure_global_sandbox(global_dir: &Path) -> EaiResult<()> {
        Self::ensure_global_sandbox_locally(global_dir)
    }
}

#[path = "runtime_shared.rs"]
mod shared;
pub use shared::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_config::{merge_missing_registry_defaults, DynamicRegistry, DynamicValue};
    use std::collections::HashMap;

    #[test]
    fn audit_log_tail_spans_chunks_and_survives_invalid_utf8() {
        let ws = std::env::temp_dir().join(format!("susi-audit-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        let mut body = Vec::new();
        body.extend_from_slice(b"bad \xff byte\n");
        for i in 0..10000 {
            body.extend_from_slice(format!("{{\"type\":\"E\",\"n\":{i}}}\n").as_bytes());
        }
        assert!(body.len() > 128 * 1024);
        std::fs::write(ws.join(".susi/audit.log"), &body).unwrap();
        let tail = SusiAuditLogger::read_audit_log(&ws, 3);
        assert_eq!(
            tail,
            "{\"type\":\"E\",\"n\":9997}\n{\"type\":\"E\",\"n\":9998}\n{\"type\":\"E\",\"n\":9999}"
        );
        let all = SusiAuditLogger::read_audit_log(&ws, 20_000);
        assert_eq!(all.lines().count(), 10001);
        assert!(all.starts_with("bad"));
        let _ = std::fs::remove_dir_all(&ws);
    }

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
        assert_eq!(
            cfg.gmcp_port(),
            crate::susi_paths::ports::GMCP + crate::susi_paths::ports::env_port_offset()
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Every scalar accessor's only fallback is config.default.json itself
    /// (via get_or_bundled_default) — there is no second, Rust-literal copy
    /// of any default that could drift out of sync with it. Host-contract
    /// ports are a partial exception: accessors return
    /// `crate::susi_paths::ports` canonical base + `port_offset` — the
    /// per-port JSON fields stay documentation-only, only the single offset
    /// integer is honored.
    #[test]
    fn test_config_accessors_match_bundled_default_single_source_of_truth() {
        let default = SusiConfig::default();
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../../../config/config.default.json")).unwrap();

        // Host contract: per-port JSON fields are documentation-only and
        // must stay at the canonical base; accessors add the offset (env
        // first — a test host with SUSI_PORT_OFFSET set still asserts true).
        let offset = crate::susi_paths::ports::env_port_offset();
        assert_eq!(raw["port_offset"].as_u64().unwrap(), 0);
        assert_eq!(default.gmcp_port(), crate::susi_paths::ports::GMCP + offset);
        assert_eq!(
            raw["gmcp_port"].as_u64().unwrap() as u16,
            crate::susi_paths::ports::GMCP
        );
        assert_eq!(
            default.gmcp_http_port(),
            crate::susi_paths::ports::GMCP_HTTP + offset
        );
        assert_eq!(
            raw["gmcp_http_port"].as_u64().unwrap() as u16,
            crate::susi_paths::ports::GMCP_HTTP
        );
        assert_eq!(default.gemi_port(), crate::susi_paths::ports::GEMI + offset);
        assert_eq!(
            raw["gemi_port"].as_u64().unwrap() as u16,
            crate::susi_paths::ports::GEMI
        );
        assert_eq!(
            default.udp_discovery_port(),
            crate::susi_paths::ports::UDP_DISCOVERY + offset
        );
        assert_eq!(
            raw["udp_discovery_port"].as_u64().unwrap() as u16,
            crate::susi_paths::ports::UDP_DISCOVERY
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
        assert_eq!(
            cfg.gmcp_port(),
            crate::susi_paths::ports::GMCP + crate::susi_paths::ports::env_port_offset()
        );
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

    /// `port_offset` shifts the whole contract uniformly while the per-port
    /// keys stay inert — a second instance lands on a clean, predictable
    /// block (offset 100 → 9190–9194), and env `SUSI_PORT_OFFSET` outranks
    /// the config key.
    #[test]
    fn test_port_offset_shifts_contract_uniformly() {
        // When the env override is set in the test environment the env value
        // is authoritative — the config-key assertions below only hold with
        // the env knob unset.
        if crate::susi_paths::ports::env_port_offset() != 0 {
            return;
        }
        let mut cfg = SusiConfig::default();
        cfg.settings
            .insert("port_offset".to_string(), serde_json::json!(100));
        assert_eq!(cfg.gmcp_port(), crate::susi_paths::ports::GMCP + 100);
        assert_eq!(
            cfg.a2a_http_port(),
            crate::susi_paths::ports::A2A_HTTP + 100
        );
        assert_eq!(
            cfg.udp_discovery_port(),
            crate::susi_paths::ports::UDP_DISCOVERY + 100
        );
        // Offset saturates rather than overflowing u16.
        cfg.settings
            .insert("port_offset".to_string(), serde_json::json!(65535));
        assert_eq!(cfg.a2a_http_port(), u16::MAX);
    }

    #[test]
    fn test_susi_memory_lifecycle() {
        let ws = Path::new("test_mem");
        let _ = fs::create_dir_all(ws);
        SusiMemory::save_interaction(ws, "hello", "world", "test");
        let memory_file = ws.join(".susi/memory.jsonl");
        assert!(memory_file.is_file());
        SusiMemory::save_interaction(
            ws,
            "push with ghp_memoryProbe123",
            "done using sk-memoryProbe456",
            "test",
        );
        let text = fs::read_to_string(&memory_file).unwrap();
        assert!(!text.contains("ghp_memoryProbe123"), "{text}");
        assert!(!text.contains("sk-memoryProbe456"), "{text}");
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
