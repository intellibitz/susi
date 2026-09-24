//! Sandbox runtime helpers: docker exec, audit, backup, intent bundles, memory.
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::susi_config::confined_workspace_join;
use crate::susi_config::SusiConfig;
use crate::susi_config::*;

// === SANDBOX MANAGER ===
pub struct SandboxManager;

impl SandboxManager {
    pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
        // bollard lives only in the standalone `susi-sandbox` service binary;
        // consumers reach it over IPC. No local fallback — Docker isolation
        // must not be re-implemented without bollard in every feature crate.
        match crate::susi_sandbox::service::docker_exec(cmd) {
            Some(out) => Ok(out),
            None => Err(EaiError::process(
                "susi-sandbox service unreachable; docker exec requires the sandbox service on 127.0.0.1:18083 (SUSI_SANDBOX_PORT)",
            )),
        }
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
        if crate::susi_sandbox::service::ensure_global(global_dir) {
            return Ok(());
        }
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
        let global_dir = crate::susi_paths::SusiDirs::config_dir();
        let secret_patterns = SusiConfig::load(&global_dir)
            .map(|cfg| cfg.governance().secret_tokens)
            .unwrap_or_default();
        let details = crate::susi_error::redact::redact_patterns(&secret_patterns, details);
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
        if let Err(e) = crate::susi_sandbox::audit_chain::append_signed_entry(
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
                    let target_path = confined_workspace_join(workspace, fix.file_path())
                        .map_err(|e| EaiError::filesystem(e.to_string()))?;
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
                    let target_path = confined_workspace_join(workspace, fix.file_path())
                        .map_err(|e| EaiError::filesystem(e.to_string()))?;
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
            // Tail-cap rotation: the file is experience memory, not an
            // archive — past a bound it keeps the most recent entries
            // (the semantic index consumes recency, not completeness).
            // A stat per save is cheap; the rewrite only runs on the
            // crossing write.
            const EXP_CAP_BYTES: u64 = 4 * 1024 * 1024;
            const EXP_KEEP_LINES: usize = 2048;
            if fs::metadata(&exp_file).map(|m| m.len()).unwrap_or(0) > EXP_CAP_BYTES {
                let keep = fs::read_to_string(&exp_file)
                    .map(|t| {
                        t.lines()
                            .rev()
                            .take(EXP_KEEP_LINES)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let tmp = exp_file.with_extension("tmp");
                if fs::write(&tmp, format!("{keep}\n")).is_ok() {
                    let _ = fs::rename(&tmp, &exp_file);
                }
            }
            if let Ok(mut f) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&exp_file)
            {
                let _ = writeln!(f, "{}", exp_entry);
            }
        }
    }
}
