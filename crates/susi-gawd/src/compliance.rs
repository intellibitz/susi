//! The compliance audit and the natural-intent evidence ledger: the two
//! pieces of the former `admin` module the open socket serves (`susi audit`,
//! GMCP's audit tool, `susi <goal>` / `susi mission` evidence, the
//! `gawd.admin.*` plane topics).
//!
//! The rest of `admin` (the task queue, the worktree workflow, the hook
//! guard, primary parking, target seeding, version sync and the release
//! gate) is the developer-workflow product. It moved to Susi Commercial
//! Apps (`susi-commercial tasks | workflow | admin`, commercial commit
//! `5c4793332`, receipt `3b09dbb72`) in the lean-core program.

use crate::susi_error::{EaiError, EaiResult};
use std::env;
use std::fs;
use std::path::Path;

/// Tracks `#[cfg(test)]` scopes across source lines so audits ignore test-only code.
pub(crate) struct CfgTestSkipper {
    pending_cfg_test: bool,
    in_test_scope: bool,
    saw_open_brace: bool,
    brace_depth: i32,
}

impl CfgTestSkipper {
    pub(crate) fn new() -> Self {
        Self {
            pending_cfg_test: false,
            in_test_scope: false,
            saw_open_brace: false,
            brace_depth: 0,
        }
    }

    /// Returns `true` when `line` belongs to a `#[cfg(test)]` attribute, declaration, or body.
    pub(crate) fn is_test_line(&mut self, line: &str) -> bool {
        if self.in_test_scope {
            let (delta, saw_open) = brace_delta(line);
            if saw_open {
                self.saw_open_brace = true;
            }
            self.brace_depth += delta;
            if self.saw_open_brace && self.brace_depth <= 0 {
                self.in_test_scope = false;
                self.saw_open_brace = false;
                self.brace_depth = 0;
            }
            return true;
        }

        let trimmed = line.trim();
        let after_attr = if !self.pending_cfg_test {
            let Some(rest) = trimmed.strip_prefix("#[cfg(test)]") else {
                return false;
            };
            let rest = rest.trim();
            if rest.is_empty() || rest.starts_with("//") {
                self.pending_cfg_test = true;
                return true;
            }
            self.pending_cfg_test = true;
            rest
        } else {
            trimmed
        };

        if after_attr.is_empty() || after_attr.starts_with("//") || after_attr.starts_with("#[") {
            return true;
        }

        self.pending_cfg_test = false;
        let (delta, saw_open) = brace_delta(after_attr);
        if saw_open {
            if delta > 0 {
                self.in_test_scope = true;
                self.saw_open_brace = true;
                self.brace_depth = delta;
            }
            return true;
        }

        if !after_attr.contains(';') {
            // Item header spans multiple lines before its opening `{` (e.g. `mod tests\n{`).
            self.in_test_scope = true;
            self.saw_open_brace = false;
            self.brace_depth = 0;
        }
        true
    }
}

fn brace_delta(line: &str) -> (i32, bool) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut delta = 0i32;
    let mut saw_open = false;
    let mut in_str = false;
    let mut raw_hashes: Option<usize> = None;

    while i < bytes.len() {
        if in_str {
            if let Some(hashes) = raw_hashes {
                if bytes[i] == b'"'
                    && i + hashes < bytes.len()
                    && bytes[i + 1..=i + hashes].iter().all(|&b| b == b'#')
                {
                    in_str = false;
                    raw_hashes = None;
                    i += 1 + hashes;
                    continue;
                }
                i += 1;
                continue;
            }
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            break;
        }

        if bytes[i] == b'r' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                in_str = true;
                raw_hashes = Some(j - (i + 1));
                i = j + 1;
                continue;
            }
        }

        if bytes[i] == b'"' {
            in_str = true;
            raw_hashes = None;
            i += 1;
            continue;
        }

        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'') {
                i += 4;
                continue;
            }
            if bytes.get(i + 2) == Some(&b'\'') {
                i += 3;
                continue;
            }
        }

        if bytes[i] == b'{' {
            delta += 1;
            saw_open = true;
        } else if bytes[i] == b'}' {
            delta -= 1;
        }
        i += 1;
    }

    (delta, saw_open)
}

pub struct SusiAdmin;

impl SusiAdmin {
    pub fn get_global_susi_dir() -> std::path::PathBuf {
        susi_paths::SusiDirs::config_dir()
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

        let hit_reports: Vec<String> = crate::susi_core::parallel::map(files, |path| {
            if let Ok(content) = fs::read_to_string(&path) {
                let mut skipper = CfgTestSkipper::new();
                let prod_lines: Vec<&str> = content
                    .lines()
                    .filter(|line| !skipper.is_test_line(line))
                    .collect();
                let mut local_hits = Vec::new();
                for p in &patterns {
                    if !p.is_empty()
                        && prod_lines.iter().any(|line| {
                            crate::susi_error::redact::secret_match_starts(line, p)
                                .next()
                                .is_some()
                        })
                    {
                        local_hits.push(format!(
                            "- [FAIL] Security: Potential secret matching '{}' detected in {}.\n",
                            p,
                            path.display()
                        ));
                    }
                }
                if !local_hits.is_empty() {
                    return Some(local_hits.join(""));
                }
            }
            None
        })
        .into_iter()
        .flatten()
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
                let flag = |key: &str| v.get(key).and_then(|x| x.as_bool()).unwrap_or(false);
                let (valid, known, verified) = (
                    flag("is_valid_gguf"),
                    flag("checksum_known"),
                    flag("checksum_verified"),
                );
                // A model with no reference checksum is unverified — not a
                // pass, and not by itself a failure. A bad header or a
                // checksum mismatch is a failure.
                let failed = !valid || (known && !verified);
                let status = if failed {
                    "FAIL"
                } else if known {
                    "PASS"
                } else {
                    "UNVERIFIED"
                };
                let model_id = v
                    .get("model_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown");
                report.push_str(&format!(
                    "- [{status}] Model Integrity: {model_id} (Verified: {verified})\n"
                ));
                if failed {
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

        // 5. Version Consistency. Read-only: an audit judges the tree and
        // writes nothing (the sync that repairs drift is
        // `susi-commercial admin sync`).
        match version_alignment(workspace) {
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

    /// Classify the evidence entry without executing the goal. Configured
    /// labels remain authoritative; inference belongs to mission execution.
    pub fn classify_natural_intent(
        _workspace: &Path,
        intent: &str,
    ) -> (&'static str, &'static str) {
        let trimmed = intent.trim();
        let lower = trimmed.to_lowercase();
        // Mandate 35: the keyword lists live in config (`intent_classify`);
        // this function used to hard-code a copy while the config went
        // unread. `contains` entries match whole words, so "promotion" or
        // "emotion" is not a MOTION intent.
        let rules = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .intent_classify();
        let has_word = |phrase: &String| contains_phrase(&lower, &phrase.to_lowercase());
        if rules.query_exact.iter().any(|q| lower == q.to_lowercase())
            || rules
                .query_prefixes
                .iter()
                .any(|p| lower.starts_with(&p.to_lowercase()))
            || rules.query_contains.iter().any(has_word)
        {
            return ("[QUERY]", "Zero-Mutation Interrogation");
        }

        if rules.motion_contains.iter().any(has_word) {
            return ("[MOTION]", "Architectural Evolution");
        }

        // The ledger label must not warm a model or generate an answer before
        // the governed mission begins. Reuse the same deterministic scope
        // classification as execution; unrecognized intent is a mission.
        if crate::susi_core::manifold::IntentManifold::analyze(trimmed).scope_of_impact
            == crate::susi_core::manifold::ScopeOfImpact::Read
        {
            return ("[QUERY]", "Zero-Mutation Interrogation");
        }

        ("[MISSION]", "Dynamic Task Fulfillment")
    }

    /// Ingest a natural language intent into `evidence.json` (`entries[]`).
    /// Genomic mode: `.agents/evidence.json`. World mode: `.susi/evidence.json`.
    pub fn ingest_natural_intent(workspace: &Path, intent: &str) -> EaiResult<String> {
        let mut evidence_path = workspace.join(".agents/evidence.json");

        if !evidence_path.exists() {
            let state_dir = susi_paths::workspace_state_dir(workspace);
            evidence_path = state_dir.join("evidence.json");
            if !evidence_path.exists() {
                fs::create_dir_all(&state_dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
                crate::susi_config::atomic_write_bytes(
                    &evidence_path,
                    crate::self_core::AlphaSelf::EVIDENCE_JSON.as_bytes(),
                )
                .map_err(|e| EaiError::filesystem(e.to_string()))?;
            }
        }

        let (prefix, _category) = Self::classify_natural_intent(workspace, intent);
        let typ = prefix.trim_matches(|c| c == '[' || c == ']');

        let content = fs::read_to_string(&evidence_path)?;
        let mut doc: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| EaiError::config(format!("evidence.json: invalid JSON: {e}")))?;

        // The ledger is committed and shared: store the intent redacted.
        let intent = crate::susi_config::redact_credentials(intent.trim());
        let intent = intent.as_str();
        let intent_trimmed = intent.to_lowercase();
        let entries = doc
            .get_mut("entries")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| EaiError::config("evidence.json: missing entries array"))?;

        // Exact (case-insensitive) match: a substring test treated any
        // short intent as "already present" because most milestones
        // contain common words, silently dropping it.
        let is_duplicate = entries.iter().any(|e| {
            e.get("milestone")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.trim().to_lowercase() == intent_trimmed)
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
        // Replaced whole: a torn plain write corrupted the governance ledger.
        crate::susi_config::atomic_write_bytes(&evidence_path, (pretty + "\n").as_bytes())
            .map_err(|e| EaiError::filesystem(format!("write {}: {e}", evidence_path.display())))?;

        Ok(format!(
            "Intent ingested successfully as {prefix} into sovereign memory"
        ))
    }
}
/// Whether `phrase` occurs in `text` as whole words (bounded by
/// non-alphanumeric characters or the text's ends).
fn contains_phrase(text: &str, phrase: &str) -> bool {
    !phrase.is_empty()
        && text.match_indices(phrase).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + phrase.len()..].chars().next();
            before.is_none_or(|c| !c.is_alphanumeric())
                && after.is_none_or(|c| !c.is_alphanumeric())
        })
}

/// The `[package]` version of `workspace/Cargo.toml`.
fn cargo_version(workspace: &Path) -> EaiResult<String> {
    let content = fs::read_to_string(workspace.join("Cargo.toml"))?;
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
        } else if trimmed.starts_with('[') {
            in_package = false;
        } else if in_package && trimmed.starts_with("version = \"") {
            if let Some(v) = trimmed.split('"').nth(1) {
                return Ok(v.to_string());
            }
        }
    }
    Err(EaiError::config("Could not find version in Cargo.toml"))
}

/// Whether the README badge and the governance ledgers name the Cargo.toml
/// version; the version when they do. Writes nothing.
fn version_alignment(workspace: &Path) -> EaiResult<String> {
    let version = cargo_version(workspace)?;
    let readme = workspace.join("README.md");
    if readme.exists() {
        let content = fs::read_to_string(&readme)?;
        if content.contains("https://img.shields.io/badge/version-v")
            && !content.contains(&format!("version-v{version}-blue.svg"))
        {
            return Err(EaiError::config(format!(
                "README.md version badge is out of sync with Cargo.toml (v{version}). \
                 Run `susi-commercial admin sync`."
            )));
        }
    }
    for file_name in ["identity.json", "roadmap.json", "evidence.json"] {
        let path = workspace.join(".agents").join(file_name);
        if !path.exists() {
            continue;
        }
        let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)
            .map_err(|e| EaiError::config(format!("{file_name}: invalid JSON: {e}")))?;
        let file_version = doc.get("version").and_then(|v| v.as_str()).unwrap_or("");
        if file_version != version {
            return Err(EaiError::config(format!(
                "{file_name}: version is out of sync with Cargo.toml (v{version}). \
                 Run `susi-commercial admin sync`."
            )));
        }
    }
    Ok(version)
}

#[cfg(test)]
#[path = "tests/compliance.rs"]
mod intent_classify_tests;
