//! Per-worker write isolation for parallel DAG dispatch (T-DEVIN-6).
//!
//! Ready DAG nodes run generated shell commands concurrently. They used
//! to share one directory, so ordinary file writes collided — same-path
//! edits silently lost, interleaved reads of half-written files. Git
//! hooks cannot help: they gate commits, not the writes themselves.
//!
//! Each mutating worker now runs in a [`WorkerScope`]: a synced snapshot
//! of the mission workspace under its own directory — the worker's
//! claimed scope. Writes stay private until a sequential
//! [`WriterIsolation::fold`] merges each finished scope back into the
//! shared workspace. A write whose path another folded scope already
//! changed differently surfaces as [`FoldOutcome::Conflict`] for review;
//! unrelated paths still apply.

use crate::susi_error::{EaiError, EaiResult};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Snapshot entries skipped — VCS internals, build output, and the
/// isolation layer's own state must never recurse into a scope.
const EXCLUDED: &[&str] = &[".git", "target", "node_modules"];
/// `.susi` subtrees owned by the orchestrator thread, not workers:
/// `dag-scopes` is where scopes live (recursion), `missions` is written
/// by the dispatch loop between batches.
const EXCLUDED_SUSI: &[&str] = &["dag-scopes", "missions"];

/// Content signature: length + SHA-256, enough to detect every drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sig {
    len: u64,
    hash: [u8; 32],
}

fn sig_of(path: &Path) -> EaiResult<Sig> {
    let bytes = std::fs::read(path)
        .map_err(|e| EaiError::io(format!("writer isolation: read {}: {e}", path.display())))?;
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    Ok(Sig {
        len: bytes.len() as u64,
        hash,
    })
}

fn is_excluded(rel: &Path, skip_all_susi: bool) -> bool {
    let mut parts = rel.iter().map(|p| p.to_string_lossy().to_string());
    let Some(first) = parts.next() else {
        return false;
    };
    if EXCLUDED.contains(&first.as_str()) {
        return true;
    }
    if first == ".susi" {
        if skip_all_susi {
            // Payload revision: `.susi` is orchestrator metadata (receipts,
            // ledgers), never the worker's deliverable.
            return true;
        }
        if let Some(second) = parts.next() {
            return EXCLUDED_SUSI.contains(&second.as_str());
        }
    }
    false
}

/// Recursive file listing relative to `root`, honoring [`is_excluded`].
fn walk(root: &Path, rel: &Path, skip_all_susi: bool, out: &mut Vec<PathBuf>) -> EaiResult<()> {
    let dir = root.join(rel);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(EaiError::io(format!(
                "writer isolation: list {}: {e}",
                dir.display()
            )));
        }
    };
    for entry in rd {
        let entry =
            entry.map_err(|e| EaiError::io(format!("writer isolation: read dir entry: {e}")))?;
        let name = entry.file_name();
        let child = rel.join(&name);
        if is_excluded(&child, skip_all_susi) {
            continue;
        }
        let ty = entry
            .file_type()
            .map_err(|e| EaiError::io(format!("writer isolation: stat entry: {e}")))?;
        if ty.is_dir() {
            walk(root, &child, skip_all_susi, out)?;
        } else if ty.is_file() || ty.is_symlink() {
            out.push(child);
        }
    }
    Ok(())
}

/// Snapshot manifest of a directory: relative path → content signature.
fn manifest_of(root: &Path) -> EaiResult<BTreeMap<String, Sig>> {
    let mut files = Vec::new();
    walk(root, Path::new(""), false, &mut files)?;
    let mut manifest = BTreeMap::new();
    for rel in files {
        let sig = sig_of(&root.join(&rel))?;
        manifest.insert(rel.to_string_lossy().replace('\\', "/"), sig);
    }
    Ok(manifest)
}

/// Content revision of a workspace: a SHA-256 over the sorted manifest of
/// *payload* files (`.susi` orchestrator state excluded, like `.git`).
/// The non-git analogue of a commit SHA — two identical trees share a
/// revision; any payload byte difference produces a different one.
pub fn revision(root: &Path) -> EaiResult<String> {
    let mut files = Vec::new();
    walk(root, Path::new(""), true, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for rel in files {
        let sig = sig_of(&root.join(&rel))?;
        let norm = rel.to_string_lossy().replace('\\', "/");
        h.update(norm.as_bytes());
        h.update([0]);
        h.update(sig.len.to_le_bytes());
        h.update(sig.hash);
    }
    let digest: [u8; 32] = h.finalize().into();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// One worker's claimed scope: a private synced copy of the mission
/// workspace plus the snapshot manifest its writes are diffed against.
#[derive(Debug)]
pub struct WorkerScope {
    /// Owning worker identity (matches the lease owner).
    pub owner: String,
    /// The scope's private directory — every tool call runs here.
    pub dir: PathBuf,
    /// rel-path → signature at snapshot time.
    manifest: BTreeMap<String, Sig>,
}

/// Outcome of folding a finished scope back into the shared workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldOutcome {
    /// All writes applied.
    Clean {
        /// Paths written or created in the workspace.
        wrote: Vec<String>,
        /// Paths deleted from the workspace.
        deleted: Vec<String>,
    },
    /// Some paths were already changed by another folded scope; those
    /// paths were NOT overwritten — they need review. Unrelated writes
    /// did apply (listed in `wrote`/`deleted` inside `partial`).
    Conflict {
        /// Overlapping diverged paths.
        paths: BTreeSet<String>,
    },
}

/// Isolation boundary for one mission workspace. Scope creation is
/// sequential (called before the parallel batch); folds are sequential
/// (after the batch), so conflict detection is deterministic.
pub struct WriterIsolation {
    /// The shared mission workspace.
    base: PathBuf,
    /// `<base>/.susi/dag-scopes` — every scope lives under here.
    scopes_root: PathBuf,
}

impl WriterIsolation {
    /// Open the isolation boundary for `base` (the mission workspace).
    pub fn new(base: &Path) -> EaiResult<Self> {
        let scopes_root = base.join(".susi").join("dag-scopes");
        std::fs::create_dir_all(&scopes_root).map_err(|e| {
            EaiError::io(format!(
                "writer isolation: create {}: {e}",
                scopes_root.display()
            ))
        })?;
        Ok(Self {
            base: base.to_path_buf(),
            scopes_root,
        })
    }

    /// Retire private scopes from an earlier coordinator generation before
    /// redispatch.  Renaming, rather than reusing, the directory prevents a
    /// delayed old process from writing into a replacement worker's scope;
    /// the retired tree is never eligible for [`Self::fold`].
    pub fn retire_stale_scopes(&self, generation: &str) -> EaiResult<Vec<PathBuf>> {
        let mut retired = Vec::new();
        let entries = std::fs::read_dir(&self.scopes_root).map_err(|e| {
            EaiError::io(format!(
                "writer isolation: list {}: {e}",
                self.scopes_root.display()
            ))
        })?;
        for entry in entries {
            let entry = entry
                .map_err(|e| EaiError::io(format!("writer isolation: read scope entry: {e}")))?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|e| EaiError::io(format!("writer isolation: stat scope: {e}")))?
                .is_dir()
            {
                continue;
            }
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if name.starts_with("stale-") {
                continue;
            }
            let retired_path = self.scopes_root.join(format!("stale-{generation}-{name}"));
            std::fs::rename(&path, &retired_path).map_err(|e| {
                EaiError::io(format!("writer isolation: retire {}: {e}", path.display()))
            })?;
            retired.push(retired_path);
        }
        Ok(retired)
    }

    /// Create a synced private scope for `owner`: a full copy of the
    /// workspace minus exclusions, with a signature manifest.
    pub fn scope(&self, owner: &str) -> EaiResult<WorkerScope> {
        let dir = self.scopes_root.join(owner);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| {
                EaiError::io(format!("writer isolation: reset {}: {e}", dir.display()))
            })?;
        }
        std::fs::create_dir_all(&dir).map_err(|e| {
            EaiError::io(format!("writer isolation: create {}: {e}", dir.display()))
        })?;
        let manifest = manifest_of(&self.base)?;
        // Materialize the snapshot: copy every tracked file.
        for rel in manifest.keys() {
            let src = self.base.join(rel);
            let dst = dir.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EaiError::io(format!(
                        "writer isolation: create {}: {e}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::copy(&src, &dst).map_err(|e| {
                EaiError::io(format!("writer isolation: snapshot {}: {e}", src.display()))
            })?;
        }
        Ok(WorkerScope {
            owner: owner.to_string(),
            dir,
            manifest,
        })
    }

    /// Diff the scope against its snapshot and CAS-merge every change
    /// into the shared workspace. Same-path writes that diverged are
    /// refused (never overwritten) and reported as conflicts.
    pub fn fold(&self, scope: &WorkerScope) -> EaiResult<FoldOutcome> {
        if scope
            .dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("stale-"))
        {
            return Err(EaiError::governance(
                "writer isolation: stale worker scope cannot be folded",
            ));
        }
        if !scope.dir.is_dir() {
            let owner = scope
                .dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let retired = scope.dir.parent().is_some_and(|root| {
                std::fs::read_dir(root)
                    .ok()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(&format!("-{owner}"))
                            && entry.file_name().to_string_lossy().starts_with("stale-")
                    })
            });
            if retired {
                return Err(EaiError::governance(
                    "writer isolation: stale worker scope cannot be folded",
                ));
            }
            return Err(EaiError::governance(
                "writer isolation: missing worker scope cannot be folded",
            ));
        }
        let now = manifest_of(&scope.dir)?;
        let mut created_or_modified: Vec<(String, Sig)> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();
        for (rel, sig) in &now {
            match scope.manifest.get(rel) {
                Some(old) if old == sig => {}
                _ => created_or_modified.push((rel.clone(), *sig)),
            }
        }
        for rel in scope.manifest.keys() {
            if !now.contains_key(rel) {
                deleted.push(rel.clone());
            }
        }

        // First pass: classify without touching the base.
        let mut conflicts = BTreeSet::new();
        let mut apply_writes = Vec::new();
        let mut apply_deletes = Vec::new();
        for (rel, new_sig) in &created_or_modified {
            let base_path = self.base.join(rel);
            let base_sig = if base_path.exists() {
                Some(sig_of(&base_path)?)
            } else {
                None
            };
            match (scope.manifest.get(rel), base_sig) {
                // New file: ok unless base gained a different version.
                (None, None) => apply_writes.push(rel.clone()),
                (None, Some(b)) if b == *new_sig => {}
                (None, Some(_)) => {
                    conflicts.insert(rel.clone());
                }
                // Modified: ok only when base still matches the snapshot.
                (Some(old), Some(b)) if b == *old => apply_writes.push(rel.clone()),
                (Some(_), Some(b)) if b == *new_sig => {}
                (Some(_), _) => {
                    conflicts.insert(rel.clone());
                }
            }
        }
        for rel in &deleted {
            let base_path = self.base.join(rel);
            match (scope.manifest.get(rel), base_path.exists()) {
                (Some(old), true) => {
                    let b = sig_of(&base_path)?;
                    if b == *old {
                        apply_deletes.push(rel.clone());
                    } else {
                        conflicts.insert(rel.clone());
                    }
                }
                (Some(_), false) => {}
                (None, _) => {}
            }
        }

        // Second pass: apply non-conflicting changes.
        for rel in &apply_writes {
            let src = scope.dir.join(rel);
            let dst = self.base.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EaiError::io(format!(
                        "writer isolation: create {}: {e}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::copy(&src, &dst).map_err(|e| {
                EaiError::io(format!("writer isolation: fold {}: {e}", dst.display()))
            })?;
        }
        for rel in &apply_deletes {
            let path = self.base.join(rel);
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| {
                    EaiError::io(format!("writer isolation: remove {}: {e}", path.display()))
                })?;
            }
        }

        if conflicts.is_empty() {
            Ok(FoldOutcome::Clean {
                wrote: apply_writes,
                deleted: apply_deletes,
            })
        } else {
            Ok(FoldOutcome::Conflict { paths: conflicts })
        }
    }

    /// Remove a scope's directory (best-effort cleanup after the batch).
    pub fn cleanup(&self, scope: &WorkerScope) {
        let _ = std::fs::remove_dir_all(&scope.dir);
    }
}

#[cfg(test)]
#[path = "tests/writer_isolation.rs"]
mod tests;
