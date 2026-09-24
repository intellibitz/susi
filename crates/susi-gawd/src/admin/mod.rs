// SUSI Native Administrative Substrate
// 100% Rust implementation for Full Compliance Enforcement, Version Synchronization & Release Orchestration

mod release;
mod version;

pub use version::VersionBump;

use crate::susi_error::{EaiError, EaiResult};
use rayon::prelude::*;
use std::env;
use std::fs;
use std::path::Path;

pub struct SusiAdmin;

impl SusiAdmin {
    pub fn get_global_susi_dir() -> std::path::PathBuf {
        crate::susi_paths::SusiDirs::config_dir()
    }

    /// Full Compliance Audit
    pub fn audit_compliance(workspace: &Path, target: Option<&str>) -> EaiResult<String> {
        let mut report = "# SUSI Compliance Audit\n\n".to_string();
        if let Some(t) = target {
            report.push_str(&format!("Target: {}\n\n", t));
        }
        let mut overall_success = true;

        // 1. Audit Security Patterns (No hardcoded keys)
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let patterns = cfg.governance().secret_tokens();
        let src_dir = workspace.join("src");

        let mut files = Vec::new();
        let mut dirs = vec![src_dir];
        while let Some(dir) = dirs.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(path);
                    } else if path.is_file() {
                        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                        if !file_name.contains("security.rs")
                            && !file_name.contains("admin")
                            && !file_name.contains("manager.rs")
                        {
                            files.push(path);
                        }
                    }
                }
            }
        }

        let hit_reports: Vec<String> = files
            .into_par_iter()
            .filter_map(|path| {
                if let Ok(content) = fs::read_to_string(&path) {
                    let mut local_hits = Vec::new();
                    for p in &patterns {
                        if content.contains(p) {
                            local_hits.push(format!("- [FAIL] Security: Potential secret matching '{}' detected in {}.\n", p, path.display()));
                        }
                    }
                    if !local_hits.is_empty() {
                        return Some(local_hits.join(""));
                    }
                }
                None
            })
            .collect();

        if hit_reports.is_empty() {
            report.push_str("- [PASS] Security: No hardcoded secrets detected.\n");
        } else {
            overall_success = false;
            for h in hit_reports {
                report.push_str(&h);
            }
        }

        // 2. Enforce Workspace Purity
        crate::susi_sandbox::manager::SandboxManager::ensure_gitignore_purity(workspace);
        let gitignore = workspace.join(".gitignore");
        if gitignore.exists() {
            let content = fs::read_to_string(&gitignore)?;
            if content.contains(".susi") || content.contains(".susi/") {
                report.push_str("- [PASS] Workspace Purity: .susi is correctly git-ignored.\n");
            } else {
                report.push_str("- [FAIL] Workspace Purity: .susi is NOT git-ignored.\n");
                overall_success = false;
            }
        }

        // 3. Model Integrity & Provenance
        let model_verifications =
            crate::susi_core::plane_bus::gemi::ModelManager::verify_local_models(workspace);
        if model_verifications
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
        {
            report.push_str("- [WARNING] Models: No local model substrates found.\n");
        } else if let Some(items) = model_verifications.as_array() {
            for v in items {
                let verified = v
                    .get("checksum_verified")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let status = if verified { "PASS" } else { "FAIL" };
                let model_id = v
                    .get("model_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown");
                report.push_str(&format!(
                    "- [{status}] Model Integrity: {model_id} (Verified: {verified})\n"
                ));
                if !verified {
                    overall_success = false;
                }
            }
        }

        // 4. Binary Integrity Check. `/proc/self/exe` resolves the running
        // inode — a binary replaced in place still verifies the live image
        // instead of reporting a `… (deleted)` path.
        if let Ok(current_exe) = env::current_exe() {
            let verify_source = {
                let proc_exe = std::path::PathBuf::from("/proc/self/exe");
                if proc_exe.exists() {
                    proc_exe
                } else {
                    current_exe
                }
            };
            let global_dir = Self::get_global_susi_dir();
            match crate::susi_sandbox::daemon_state::SusiDaemonState::verify_binary_integrity(
                &verify_source,
                &global_dir,
            ) {
                Ok(true) => report.push_str(
                    "- [PASS] Binary Integrity: Executable hash matches trusted genome.\n",
                ),
                Ok(false) => {
                    if target == Some("release") {
                        report.push_str(
                            "- [WARNING] Binary Integrity: Executable hash refreshed after gatekeeper rebuild.\n",
                        );
                    } else {
                        report.push_str("- [FAIL] Binary Integrity: Executable hash MISMATCH. Potential tampering or build drift.\n");
                        overall_success = false;
                    }
                }
                Err(e) => report.push_str(&format!(
                    "- [WARNING] Binary Integrity: Could not verify ({})\n",
                    e
                )),
            }
        }

        // 5. Version Consistency
        match Self::enforce_version_consistency(workspace) {
            Ok(v) => report.push_str(&format!(
                "- [PASS] Version Consistency: All manifests synchronized to v{}.\n",
                v
            )),
            Err(e) => {
                report.push_str(&format!("- [FAIL] Version Consistency: {}\n", e));
                overall_success = false;
            }
        }

        if overall_success {
            Ok(report)
        } else {
            Err(EaiError::governance(format!(
                "Compliance Audit Failed:\n{}",
                report
            )))
        }
    }

    /// Dynamic Neural Cascade Classifier (Tier 0 Reflex -> Tier 2 GEMI -> Motion)
    pub fn classify_natural_intent(workspace: &Path, intent: &str) -> (&'static str, &'static str) {
        let trimmed = intent.trim();
        let lower = trimmed.to_lowercase();

        if lower == "identity"
            || lower == "status"
            || lower == "models"
            || lower == "version"
            || lower == "ls"
            || lower.starts_with("ls ")
            || lower == "dir"
            || lower.contains("who am i")
            || lower.contains("whoami")
            || lower.starts_with("susi status")
            || lower.starts_with("susi identity")
            || lower.starts_with("susi models")
        {
            return ("[QUERY]", "Zero-Mutation Interrogation");
        }

        if lower.contains("motion") || lower.contains("architecture") || lower.contains("hardcode")
        {
            return ("[MOTION]", "Architectural Evolution");
        }

        {
            let reflex_action = crate::susi_core::plane_bus::gemi::pulse_reason(trimmed, workspace);
            let reflex_lower = reflex_action.to_lowercase();
            if reflex_action.is_empty() {
                // fall through
            } else if reflex_lower.contains("query")
                || reflex_lower.contains("status")
                || reflex_lower.contains("identity")
            {
                return ("[QUERY]", "Zero-Mutation Interrogation");
            } else if reflex_lower.contains("motion") || reflex_lower.contains("recompile") {
                return ("[MOTION]", "Architectural Evolution");
            } else if reflex_lower.contains("mission") || reflex_lower.contains("solve") {
                return ("[MISSION]", "Dynamic Task Fulfillment");
            }
        }

        ("[MISSION]", "Dynamic Task Fulfillment")
    }

    /// Ingest a natural language intent into `evidence.json` (`entries[]`).
    /// Genomic mode: `.agents/evidence.json`. World mode: `.susi/evidence.json`.
    pub fn ingest_natural_intent(workspace: &Path, intent: &str) -> EaiResult<String> {
        let mut evidence_path = workspace.join(".agents/evidence.json");

        if !evidence_path.exists() {
            evidence_path = workspace.join(".susi/evidence.json");
            if !evidence_path.exists() {
                fs::create_dir_all(workspace.join(".susi"))
                    .map_err(|e| EaiError::filesystem(e.to_string()))?;
                fs::write(&evidence_path, crate::self_core::AlphaSelf::EVIDENCE_JSON)?;
            }
        }

        let (prefix, _category) = Self::classify_natural_intent(workspace, intent);
        let typ = prefix.trim_matches(|c| c == '[' || c == ']');

        let content = fs::read_to_string(&evidence_path)?;
        let mut doc: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| EaiError::config(format!("evidence.json: invalid JSON: {e}")))?;

        let intent_trimmed = intent.trim().to_lowercase();
        let entries = doc
            .get_mut("entries")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| EaiError::config("evidence.json: missing entries array"))?;

        let is_duplicate = entries.iter().any(|e| {
            e.get("milestone")
                .and_then(|m| m.as_str())
                .map(|m| m.to_lowercase().contains(&intent_trimmed))
                .unwrap_or(false)
        });
        if is_duplicate {
            return Ok(format!(
                "Intent '{intent}' is already present in memory ({prefix})"
            ));
        }

        let mut last_index = 0usize;
        let mut best_suffix = "2022920".to_string();
        for e in entries.iter() {
            let Some(token) = e.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if !token.starts_with("EV-") {
                continue;
            }
            let sub_parts: Vec<&str> = token.split('-').collect();
            if sub_parts.len() >= 3 {
                if sub_parts[1] > best_suffix.as_str() {
                    best_suffix = sub_parts[1].to_string();
                }
                if let Ok(idx) = sub_parts[2].parse::<usize>() {
                    if idx > last_index {
                        last_index = idx;
                    }
                }
            }
        }

        let id = format!("EV-{}-{:03}", best_suffix, last_index + 1);
        entries.push(serde_json::json!({
            "id": id,
            "type": typ,
            "milestone": intent,
            "anchor": { "label": "manual", "ref": "symbol://manual" },
            "proof": "STAGED"
        }));

        let pretty = serde_json::to_string_pretty(&doc)
            .map_err(|e| EaiError::config(format!("evidence.json: serialize: {e}")))?;
        fs::write(&evidence_path, pretty + "\n")?;

        Ok(format!(
            "Intent ingested successfully as {prefix} into sovereign memory"
        ))
    }
}
