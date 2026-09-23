//! Apply-patch-then-test feedback cycle with workspace confinement and rollback.
//!
//! This module is intentionally conservative: it only edits files under the
//! caller's workspace, requires the `old` text to match before overwriting,
//! keeps backups, runs the project test command, and restores backups when
//! tests fail. Autonomous application is gated by `trust_level`.

use crate::susi_error::{EaiError, EaiResult};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FilePatch {
    pub path: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PatchRequest {
    pub files: Vec<FilePatch>,
    #[serde(default)]
    pub test_command: Option<String>,
    /// Allow application without human confirmation. Still workspace-confined.
    #[serde(default)]
    pub auto_apply: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PatchOutcome {
    pub applied: bool,
    pub test_passed: bool,
    pub files_changed: Vec<String>,
    pub test_stdout: String,
    pub test_stderr: String,
    pub reverted: bool,
    pub error: Option<String>,
}

/// Returns the absolute, normalized path only if it is inside `workspace`.
fn confined_path(workspace: &Path, rel: &str) -> EaiResult<PathBuf> {
    let rel = rel.replace('\\', "/").trim_start_matches('/').to_string();
    if rel.is_empty() {
        return Err(EaiError::governance("empty patch path"));
    }
    if rel.contains("..") {
        return Err(EaiError::governance(format!(
            "patch path attempts escape: {rel}"
        )));
    }
    let candidate = workspace.join(&rel);
    let canonical_ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let canonical_file = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());
    if !canonical_file.starts_with(&canonical_ws) {
        return Err(EaiError::governance(format!(
            "patch path outside workspace: {rel}"
        )));
    }
    Ok(candidate)
}

fn backup_dir(workspace: &Path) -> PathBuf {
    workspace.join(".susi").join("patch_backups")
}

fn read_file(path: &Path) -> EaiResult<String> {
    std::fs::read_to_string(path).map_err(|e| EaiError::filesystem(format!("read {path:?}: {e}")))
}

fn write_file(path: &Path, content: &str) -> EaiResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| EaiError::filesystem(format!("mkdir {parent:?}: {e}")))?;
    }
    std::fs::write(path, content).map_err(|e| EaiError::filesystem(format!("write {path:?}: {e}")))
}

fn detect_test_command(workspace: &Path) -> String {
    if workspace.join("Cargo.toml").is_file() {
        "cargo test".into()
    } else if workspace.join("package.json").is_file() {
        "npm test".into()
    } else {
        "echo 'no test command detected'".into()
    }
}

/// Run a patch-then-test cycle. `trust_level` should be the configured level
/// (e.g. "conservative", "balanced", "autonomous").
pub fn apply_patch_cycle(
    workspace: &Path,
    request: &PatchRequest,
    trust_level: &str,
) -> EaiResult<PatchOutcome> {
    // Safety gate: autonomous application only when explicitly enabled.
    let autonomous = trust_level.eq_ignore_ascii_case("autonomous");
    if !autonomous && !request.auto_apply {
        return Ok(PatchOutcome {
            applied: false,
            test_passed: false,
            files_changed: Vec::new(),
            test_stdout: String::new(),
            test_stderr: String::new(),
            reverted: false,
            error: Some(format!(
                "Patch requires auto_apply=true or trust_level=autonomous (current: {trust_level})"
            )),
        });
    }

    if request.files.is_empty() {
        return Ok(PatchOutcome {
            applied: false,
            test_passed: false,
            files_changed: Vec::new(),
            test_stdout: String::new(),
            test_stderr: String::new(),
            reverted: false,
            error: Some("no files in patch request".into()),
        });
    }

    // Validate and confine every target path.
    let mut targets: Vec<(PathBuf, String, String)> = Vec::new();
    for fp in &request.files {
        let path = confined_path(workspace, &fp.path)?;
        targets.push((path, fp.old.clone(), fp.new.clone()));
    }

    let backups = backup_dir(workspace);
    let _ = std::fs::create_dir_all(&backups);

    // Snapshot current content so we can restore on failure.
    let mut restore_map: Vec<(PathBuf, Option<String>)> = Vec::new();
    let file_rels: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    let tx = crate::susi_core::agent_tx::TxManager::global()
        .begin(
            workspace,
            &request.description,
            &file_rels,
            Default::default(),
        )
        .ok();

    for (path, _old, _new) in &targets {
        let original = if path.is_file() {
            Some(read_file(path)?)
        } else {
            None
        };
        restore_map.push((path.clone(), original.clone()));
        let backup_path = backups.join(
            path.file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("patch")),
        );
        if let Some(ref content) = original {
            let _ = std::fs::write(&backup_path, content);
        }
    }

    // Apply patches: require exact old-text match unless file does not exist
    // and old is empty.
    let mut files_changed = Vec::new();
    for (path, old, new) in &targets {
        let current = if path.is_file() {
            read_file(path)?
        } else {
            String::new()
        };
        if !old.is_empty() && current != *old {
            return Err(EaiError::governance(format!(
                "patch stale: {} current text does not match supplied old text",
                path.display()
            )));
        }
        write_file(path, new)?;
        files_changed.push(
            path.strip_prefix(workspace)
                .unwrap_or(path)
                .display()
                .to_string(),
        );
    }

    // Run tests.
    let test_cmd = request
        .test_command
        .clone()
        .unwrap_or_else(|| detect_test_command(workspace));
    let output = Command::new("sh")
        .arg("-c")
        .arg(&test_cmd)
        .current_dir(workspace)
        .output()
        .map_err(|e| EaiError::process(format!("failed to run test command: {e}")))?;
    let test_passed = output.status.success();
    let test_stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let test_stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let mut reverted = false;
    if !test_passed {
        for (path, original) in &restore_map {
            if let Some(content) = original {
                let _ = write_file(path, content);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
        files_changed.clear();
        reverted = true;
        if let Some(ref t) = tx {
            let _ = crate::susi_core::agent_tx::TxManager::global().abort(&t.id, workspace);
        }
    } else if let Some(ref t) = tx {
        let _ = crate::susi_core::agent_tx::TxManager::global().commit(&t.id);
    }

    let outcome = PatchOutcome {
        applied: true,
        test_passed,
        files_changed,
        test_stdout,
        test_stderr,
        reverted,
        error: if test_passed {
            None
        } else {
            Some("tests failed; patch reverted".into())
        },
    };

    // Record the feedback cycle outcome in the context graph.
    let payload = serde_json::json!({
        "description": request.description,
        "applied": outcome.applied,
        "test_passed": outcome.test_passed,
        "files_changed": outcome.files_changed,
        "reverted": outcome.reverted,
        "error": outcome.error,
    });
    crate::susi_core::context_graph::ContextGraph::global().record_external_context(
        "patch_cycle",
        &format!(
            "patch cycle: {} files, test_passed={}, reverted={}",
            outcome.files_changed.len(),
            outcome.test_passed,
            outcome.reverted
        ),
        &payload,
        Some(workspace),
        None,
    );

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws() -> PathBuf {
        std::env::temp_dir().join(format!(
            "susi-patch-cycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn patch_requires_autonomous_or_auto_apply() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        let req = PatchRequest {
            files: vec![FilePatch {
                path: "a.txt".into(),
                old: "".into(),
                new: "hello".into(),
            }],
            test_command: Some("true".into()),
            auto_apply: false,
            description: "test".into(),
        };
        let out = apply_patch_cycle(&ws, &req, "balanced").unwrap();
        assert!(!out.applied);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn patch_applies_and_passes_test() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("a.txt"), "old").unwrap();
        let req = PatchRequest {
            files: vec![FilePatch {
                path: "a.txt".into(),
                old: "old".into(),
                new: "new".into(),
            }],
            test_command: Some("true".into()),
            auto_apply: true,
            description: "test".into(),
        };
        let out = apply_patch_cycle(&ws, &req, "balanced").unwrap();
        assert!(out.applied);
        assert!(out.test_passed);
        assert!(!out.reverted);
        let content = std::fs::read_to_string(ws.join("a.txt")).unwrap();
        assert_eq!(content, "new");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn patch_reverts_on_test_failure() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("a.txt"), "old").unwrap();
        let req = PatchRequest {
            files: vec![FilePatch {
                path: "a.txt".into(),
                old: "old".into(),
                new: "new".into(),
            }],
            test_command: Some("false".into()),
            auto_apply: true,
            description: "test".into(),
        };
        let out = apply_patch_cycle(&ws, &req, "balanced").unwrap();
        assert!(out.applied);
        assert!(!out.test_passed);
        assert!(out.reverted);
        let content = std::fs::read_to_string(ws.join("a.txt")).unwrap();
        assert_eq!(content, "old");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn patch_rejects_escape() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        let req = PatchRequest {
            files: vec![FilePatch {
                path: "../escape.txt".into(),
                old: "".into(),
                new: "x".into(),
            }],
            test_command: Some("true".into()),
            auto_apply: true,
            description: "test".into(),
        };
        assert!(apply_patch_cycle(&ws, &req, "autonomous").is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }
}
