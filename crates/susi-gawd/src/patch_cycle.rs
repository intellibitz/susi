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
    /// Run the apply+test inside an isolated copy of the workspace — a
    /// `PatchFence` sibling — and promote the changed files into the real
    /// tree only when the fenced run passes. `Some(id)` names the
    /// experiment; the fence sanitizes it to a single path segment, so a
    /// patch id can never escape the fences root or collide with a
    /// sibling experiment's workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolate: Option<String>,
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
    /// The fenced workspace an isolated run executed in — set only for
    /// `request.isolate`, and retained (never cleaned) so a rejected run
    /// leaves inspectable evidence rather than silent absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolated_workspace: Option<String>,
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
    // Resolve through the deepest existing ancestor so a symlinked dir or
    // leaf is judged by its real target, including for files the patch
    // creates (the old canonicalize-or-raw fallback let `dirlink/new.rs`
    // write through a symlink out of the workspace).
    crate::susi_config::confined_workspace_join(workspace, &rel)
        .map_err(|e| EaiError::governance(format!("patch path outside workspace: {rel} ({e})")))
}

fn read_file(path: &Path) -> EaiResult<String> {
    std::fs::read_to_string(path).map_err(|e| EaiError::filesystem(format!("read {path:?}: {e}")))
}

fn write_file(path: &Path, content: &str) -> EaiResult<()> {
    crate::susi_config::atomic_replace_file(path, content.as_bytes())
        .map_err(|e| EaiError::filesystem(format!("write {path:?}: {e}")))
}

/// Populate `dst` as a candidate checkout of `src`: every file is copied
/// (never hard-linked — a shared inode would leak the candidate's
/// in-place writes into the live tree before promotion), `.git`/
/// `target`/`node_modules` are skipped, and every symlink is normalized
/// to an absolute target inside the fence — or dropped when it resolves
/// outside `src`, since an escaping link would hand the fenced run a
/// write channel out of isolation.
fn stage_candidate(src: &Path, dst: &Path) -> EaiResult<()> {
    let rd = std::fs::read_dir(src)
        .map_err(|e| EaiError::filesystem(format!("fence link read {src:?}: {e}")))?;
    for entry in rd {
        let entry = entry.map_err(|e| EaiError::filesystem(format!("fence link entry: {e}")))?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".git" | "target" | "node_modules")) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let ft = entry
            .file_type()
            .map_err(|e| EaiError::filesystem(format!("fence link stat {from:?}: {e}")))?;
        if ft.is_dir() {
            std::fs::create_dir_all(&to)
                .map_err(|e| EaiError::filesystem(format!("fence mkdir {to:?}: {e}")))?;
            stage_candidate(&from, &to)?;
        } else if ft.is_symlink() {
            link_symlink(&from, src, &to, dst)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)
                .map_err(|e| EaiError::filesystem(format!("fence copy {from:?}: {e}")))?;
        }
    }
    Ok(())
}

/// Recreate a symlink inside the fence — normalized to an absolute
/// in-fence target, or dropped when it resolves outside `src`: a link
/// that escapes would hand the fenced run a write channel out of
/// isolation, and one left pointing at `src` would leak writes back
/// into the real tree.
#[cfg(unix)]
fn link_symlink(from: &Path, src: &Path, to: &Path, dst: &Path) -> EaiResult<()> {
    let text = std::fs::read_link(from)
        .map_err(|e| EaiError::filesystem(format!("fence readlink {from:?}: {e}")))?;
    let resolved = crate::patch_fence::normalize_lexically(
        &from
            .parent()
            .map_or_else(|| text.clone(), |p| p.join(&text)),
    );
    let Ok(rel) = resolved.strip_prefix(crate::patch_fence::normalize_lexically(src)) else {
        return Ok(()); // escapes the workspace: no such link in the fence
    };
    std::os::unix::fs::symlink(dst.join(rel), to)
        .map_err(|e| EaiError::filesystem(format!("fence symlink {to:?}: {e}")))
}

/// Non-unix fallback: symlinks are not recreated in the fence.
#[cfg(not(unix))]
fn link_symlink(_from: &Path, _src: &Path, _to: &Path, _dst: &Path) -> EaiResult<()> {
    Ok(())
}

/// Promote the fenced run's changed files into the real workspace under
/// its own transaction — every target is re-validated first, because the
/// tree may have drifted while the fenced apply+test executed and a
/// stale later file must not leave earlier promotes half-written.
fn promote_into(workspace: &Path, files: &[FilePatch]) -> EaiResult<Vec<String>> {
    let mut targets: Vec<(PathBuf, &str)> = Vec::new();
    for fp in files {
        let path = confined_path(workspace, &fp.path)?;
        let current = if path.is_file() {
            read_file(&path)?
        } else {
            String::new()
        };
        if current != fp.old {
            return Err(EaiError::governance(format!(
                "promote refused: {} drifted during the fenced run",
                path.display()
            )));
        }
        targets.push((path, fp.new.as_str()));
    }
    let file_rels: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let txm = crate::susi_core::agent_tx::TxManager::global();
    let tx = txm.begin(
        workspace,
        "promote fenced patch",
        &file_rels,
        Default::default(),
    )?;
    let mut changed = Vec::new();
    for (path, new) in &targets {
        if let Err(e) = write_file(path, new) {
            let _ = txm.abort(&tx.id, workspace);
            return Err(EaiError::filesystem(format!("promote write failed: {e}")));
        }
        changed.push(
            path.strip_prefix(workspace)
                .unwrap_or(path)
                .display()
                .to_string(),
        );
    }
    txm.commit(&tx.id, workspace)?;
    Ok(changed)
}

fn detect_test_command(workspace: &Path) -> String {
    // A patch to SUSI itself must clear the full gate (Mandate 48), not
    // just `cargo test`: fmt and `clippy -D warnings` failures block CI too.
    if crate::susi_core::self_build::is_susi_repo(workspace) {
        crate::susi_core::self_build::VERIFY_COMMAND.into()
    } else if workspace.join("Cargo.toml").is_file() {
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
            isolated_workspace: None,
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
            isolated_workspace: None,
        });
    }

    // Stage the execution workspace. In isolated mode the candidate runs
    // inside a PatchFence sibling — a full copy of the workspace — so the
    // apply and its test run can never write the real tree: only a
    // passing fenced run promotes its changed files back, under a fresh
    // staleness check and transaction of their own.
    let fence = match &request.isolate {
        Some(patch_id) => {
            let root = workspace
                .parent()
                .map_or_else(|| workspace.to_path_buf(), Path::to_path_buf);
            let f = crate::patch_fence::PatchFence::isolate(&root, patch_id)
                .map_err(EaiError::governance)?;
            f.ensure_isolated()
                .map_err(|e| EaiError::filesystem(format!("fence workspace create: {e}")))?;
            stage_candidate(workspace, &f.workspace)?;
            Some(f)
        }
        None => None,
    };
    let exec_ws = fence
        .as_ref()
        .map_or_else(|| workspace.to_path_buf(), |f| f.workspace.clone());

    // Validate and confine every target path, and check every file for
    // staleness before anything is written: a stale later file must not
    // leave earlier files half-applied. `old` is the full expected text; an
    // empty `old` only creates a missing (or empty) file.
    let mut targets: Vec<(PathBuf, &str)> = Vec::new();
    for fp in &request.files {
        let path = confined_path(&exec_ws, &fp.path)?;
        let current = if path.is_file() {
            read_file(&path)?
        } else {
            String::new()
        };
        if current != fp.old {
            return Err(EaiError::governance(format!(
                "patch stale: {} current text does not match supplied old text",
                path.display()
            )));
        }
        targets.push((path, fp.new.as_str()));
    }

    // The transaction snapshot is the single rollback source.
    let file_rels: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    let txm = crate::susi_core::agent_tx::TxManager::global();
    let tx = txm.begin(
        &exec_ws,
        &request.description,
        &file_rels,
        Default::default(),
    )?;
    let rollback = |cause: String| -> EaiError {
        match txm.abort(&tx.id, &exec_ws) {
            Ok(_) => EaiError::filesystem(format!("{cause}; patch reverted")),
            Err(e) => EaiError::filesystem(format!("{cause}; ROLLBACK FAILED: {e}")),
        }
    };

    let mut files_changed = Vec::new();
    for (path, new) in &targets {
        if let Err(e) = write_file(path, new) {
            return Err(rollback(e.to_string()));
        }
        files_changed.push(
            path.strip_prefix(&exec_ws)
                .unwrap_or(path)
                .display()
                .to_string(),
        );
    }

    // Run tests.
    let susi_repo = crate::susi_core::self_build::is_susi_repo(&exec_ws);
    let mut test_passed = true;
    let mut test_stdout = String::new();
    let mut test_stderr = String::new();
    if let Some(test_cmd) = request.test_command.clone() {
        let output = match Command::new("sh")
            .arg("-c")
            .arg(&test_cmd)
            .current_dir(&exec_ws)
            .output()
        {
            Ok(output) => output,
            Err(e) => return Err(rollback(format!("failed to run test command: {e}"))),
        };
        test_passed = output.status.success();
        test_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        test_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
    } else if !susi_repo {
        let test_cmd = detect_test_command(&exec_ws);
        let output = match Command::new("sh")
            .arg("-c")
            .arg(&test_cmd)
            .current_dir(&exec_ws)
            .output()
        {
            Ok(output) => output,
            Err(e) => return Err(rollback(format!("failed to run test command: {e}"))),
        };
        test_passed = output.status.success();
        test_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        test_stderr.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if susi_repo {
        match crate::repo_gate::run_repository_gate(&exec_ws) {
            Ok(report) => {
                test_passed &= report.promotion_ready();
                test_stdout.push_str(&format!("repository gate executed: {report:?}"));
            }
            Err(error) => {
                test_passed = false;
                test_stderr.push_str(&error.to_string());
            }
        }
    }

    let mut reverted = false;
    let mut error = None;
    if test_passed {
        txm.commit(&tx.id, &exec_ws)?;
        if fence.is_some() {
            // The fenced run passed: promote the same contents into the
            // real workspace — under its own staleness check and
            // transaction, since the tree may have drifted mid-run.
            files_changed = promote_into(workspace, &request.files)?;
        }
    } else {
        match txm.abort(&tx.id, &exec_ws) {
            Ok(_) => {
                files_changed.clear();
                reverted = true;
                error = Some("tests failed; patch reverted".into());
            }
            Err(e) => error = Some(format!("tests failed; ROLLBACK FAILED: {e}")),
        }
    }

    let outcome = PatchOutcome {
        applied: true,
        test_passed,
        files_changed,
        test_stdout,
        test_stderr,
        reverted,
        error,
        isolated_workspace: fence.as_ref().map(|f| f.workspace.display().to_string()),
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
            isolate: None,
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
            isolate: None,
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
    fn susi_repo_runs_the_full_gate_even_with_an_override_command() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(ws.join(".agents"));
        std::fs::write(ws.join(".agents/identity.json"), "{}").unwrap();
        std::fs::write(
            ws.join("Cargo.toml"),
            "[package]\nname = \"susi\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let _ = std::fs::create_dir_all(ws.join("src"));
        std::fs::write(ws.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
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
            isolate: None,
        };
        let out = apply_patch_cycle(&ws, &req, "balanced").unwrap();
        assert!(out.applied);
        assert!(!out.test_passed, "the override must not bypass the gate");
        assert!(out.reverted);
        assert!(out.test_stdout.contains("repository gate executed"));
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "old");
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
            isolate: None,
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
    fn stale_later_file_leaves_earlier_files_untouched() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("a.txt"), "old").unwrap();
        std::fs::write(ws.join("b.txt"), "drifted").unwrap();
        let req = PatchRequest {
            files: vec![
                FilePatch {
                    path: "a.txt".into(),
                    old: "old".into(),
                    new: "new".into(),
                },
                FilePatch {
                    path: "b.txt".into(),
                    old: "expected".into(),
                    new: "x".into(),
                },
            ],
            test_command: Some("true".into()),
            auto_apply: true,
            description: "test".into(),
            isolate: None,
        };
        assert!(apply_patch_cycle(&ws, &req, "balanced").is_err());
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn empty_old_does_not_overwrite_existing_content() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("a.txt"), "keep").unwrap();
        let req = PatchRequest {
            files: vec![FilePatch {
                path: "a.txt".into(),
                old: "".into(),
                new: "clobber".into(),
            }],
            test_command: Some("true".into()),
            auto_apply: true,
            description: "test".into(),
            isolate: None,
        };
        assert!(apply_patch_cycle(&ws, &req, "balanced").is_err());
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "keep");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn failed_test_removes_created_files() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        let req = PatchRequest {
            files: vec![FilePatch {
                path: "fresh.txt".into(),
                old: "".into(),
                new: "x".into(),
            }],
            test_command: Some("false".into()),
            auto_apply: true,
            description: "test".into(),
            isolate: None,
        };
        let out = apply_patch_cycle(&ws, &req, "balanced").unwrap();
        assert!(out.reverted);
        assert!(!ws.join("fresh.txt").exists());
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
            isolate: None,
        };
        assert!(apply_patch_cycle(&ws, &req, "autonomous").is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }
}
