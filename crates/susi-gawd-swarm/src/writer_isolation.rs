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
        manifest.insert(rel.to_string_lossy().into_owned(), sig);
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
        h.update(rel.to_string_lossy().as_bytes());
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
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wiso-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|t| t.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    fn base_ws(tag: &str) -> PathBuf {
        let base = temp_root(tag);
        std::fs::write(base.join("shared.txt"), "base\n").unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
        base
    }

    /// Disjoint parallel writes: every worker's change lands in the shared
    /// workspace after its fold.
    #[test]
    fn writer_isolation_disjoint_writes_all_fold() {
        let base = base_ws("disjoint");
        let iso = WriterIsolation::new(&base).unwrap();
        let a = iso.scope("worker-a").unwrap();
        let b = iso.scope("worker-b").unwrap();
        std::fs::write(a.dir.join("a.txt"), "from a\n").unwrap();
        std::fs::write(b.dir.join("b.txt"), "from b\n").unwrap();
        std::fs::write(b.dir.join("src/b.rs"), "pub fn b() {}\n").unwrap();
        assert_eq!(
            iso.fold(&a).unwrap(),
            FoldOutcome::Clean {
                wrote: vec!["a.txt".to_string()],
                deleted: vec![]
            }
        );
        let out = iso.fold(&b).unwrap();
        match out {
            FoldOutcome::Clean { wrote, .. } => {
                assert!(wrote.contains(&"b.txt".to_string()));
                assert!(wrote.contains(&"src/b.rs".to_string()));
            }
            FoldOutcome::Conflict { .. } => panic!("disjoint writes must not conflict"),
        }
        assert_eq!(
            std::fs::read_to_string(base.join("a.txt")).unwrap(),
            "from a\n"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("b.txt")).unwrap(),
            "from b\n"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("src/b.rs")).unwrap(),
            "pub fn b() {}\n"
        );
        iso.cleanup(&a);
        iso.cleanup(&b);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A worker's write is invisible in the base until folded — no
    /// concurrent reader ever sees a half-written file.
    #[test]
    fn writer_isolation_writes_stay_private_until_fold() {
        let base = base_ws("private");
        let iso = WriterIsolation::new(&base).unwrap();
        let scope = iso.scope("worker-x").unwrap();
        std::fs::write(scope.dir.join("draft.txt"), "wip\n").unwrap();
        // While the worker runs, the shared base is untouched.
        assert!(!base.join("draft.txt").exists());
        assert!(matches!(
            iso.fold(&scope).unwrap(),
            FoldOutcome::Clean { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(base.join("draft.txt")).unwrap(),
            "wip\n"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Two workers editing the same path: first fold wins, the second is a
    /// reviewable conflict — never a silent overwrite.
    #[test]
    fn writer_isolation_same_path_conflict_is_reviewed() {
        let base = base_ws("conflict");
        let iso = WriterIsolation::new(&base).unwrap();
        let a = iso.scope("worker-a").unwrap();
        let b = iso.scope("worker-b").unwrap();
        std::fs::write(a.dir.join("shared.txt"), "version a\n").unwrap();
        std::fs::write(b.dir.join("shared.txt"), "version b\n").unwrap();
        std::fs::write(b.dir.join("ok.txt"), "unrelated\n").unwrap();
        assert!(matches!(iso.fold(&a).unwrap(), FoldOutcome::Clean { .. }));
        match iso.fold(&b).unwrap() {
            FoldOutcome::Conflict { paths } => {
                assert!(paths.contains("shared.txt"));
            }
            FoldOutcome::Clean { .. } => panic!("diverged same-path write must conflict"),
        }
        // The conflicting path was NOT overwritten; unrelated writes landed.
        assert_eq!(
            std::fs::read_to_string(base.join("shared.txt")).unwrap(),
            "version a\n"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("ok.txt")).unwrap(),
            "unrelated\n"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// The scope is synced with the workspace at creation — a worker sees
    /// existing content, including uncommitted files.
    #[test]
    fn writer_isolation_scope_is_synced_snapshot() {
        let base = base_ws("synced");
        let iso = WriterIsolation::new(&base).unwrap();
        let scope = iso.scope("worker-s").unwrap();
        assert_eq!(
            std::fs::read_to_string(scope.dir.join("shared.txt")).unwrap(),
            "base\n"
        );
        assert_eq!(
            std::fs::read_to_string(scope.dir.join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Deleting a file in the scope deletes it in the base — unless the
    /// base version drifted, which conflicts instead of losing data.
    #[test]
    fn writer_isolation_delete_folds_and_drift_conflicts() {
        let base = base_ws("delete");
        let iso = WriterIsolation::new(&base).unwrap();
        let a = iso.scope("worker-a").unwrap();
        std::fs::remove_file(a.dir.join("src/main.rs")).unwrap();
        assert!(matches!(iso.fold(&a).unwrap(), FoldOutcome::Clean { .. }));
        assert!(!base.join("src/main.rs").exists());

        // Drift under an in-flight scope conflicts rather than deleting.
        let b = iso.scope("worker-b").unwrap();
        std::fs::write(base.join("shared.txt"), "drifted\n").unwrap();
        std::fs::remove_file(b.dir.join("shared.txt")).unwrap();
        match iso.fold(&b).unwrap() {
            FoldOutcome::Conflict { paths } => assert!(paths.contains("shared.txt")),
            FoldOutcome::Clean { .. } => panic!("delete of drifted file must conflict"),
        }
        assert_eq!(
            std::fs::read_to_string(base.join("shared.txt")).unwrap(),
            "drifted\n"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Two workers creating the same path with identical content is a
    /// clean no-op, not a conflict (idempotent convergent writes).
    #[test]
    fn writer_isolation_identical_writes_converge() {
        let base = base_ws("converge");
        let iso = WriterIsolation::new(&base).unwrap();
        let a = iso.scope("worker-a").unwrap();
        let b = iso.scope("worker-b").unwrap();
        std::fs::write(a.dir.join("new.txt"), "same\n").unwrap();
        std::fs::write(b.dir.join("new.txt"), "same\n").unwrap();
        assert!(matches!(iso.fold(&a).unwrap(), FoldOutcome::Clean { .. }));
        assert!(matches!(iso.fold(&b).unwrap(), FoldOutcome::Clean { .. }));
        assert_eq!(
            std::fs::read_to_string(base.join("new.txt")).unwrap(),
            "same\n"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Many workers writing concurrently into their own scopes never
    /// collide — the shared base sees each batch only via fold.
    #[test]
    fn writer_isolation_concurrent_writers_never_share_a_directory() {
        let base = base_ws("parallel");
        let iso = WriterIsolation::new(&base).unwrap();
        let scopes: Vec<WorkerScope> = (0..4)
            .map(|i| iso.scope(&format!("worker-{i}")).unwrap())
            .collect();
        let dirs: Vec<PathBuf> = scopes.iter().map(|s| s.dir.clone()).collect();
        assert!(dirs.iter().all(|d| d != &base));
        let handles: Vec<_> = dirs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let d = d.clone();
                std::thread::spawn(move || {
                    for n in 0..8 {
                        std::fs::write(d.join(format!("w{i}-{n}.txt")), format!("{i}:{n}\n"))
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // Not one worker file leaked into the base mid-flight.
        assert!(std::fs::read_dir(&base).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("w")));
        for scope in &scopes {
            assert!(matches!(
                iso.fold(scope).unwrap(),
                FoldOutcome::Clean { .. }
            ));
            iso.cleanup(scope);
        }
        for i in 0..4 {
            for n in 0..8 {
                assert_eq!(
                    std::fs::read_to_string(base.join(format!("w{i}-{n}.txt"))).unwrap(),
                    format!("{i}:{n}\n")
                );
            }
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `.git`, `target` and the scopes' own subtree never enter a snapshot.
    #[test]
    fn writer_isolation_excludes_vcs_build_and_self() {
        let base = base_ws("excl");
        std::fs::create_dir_all(base.join(".git/objects")).unwrap();
        std::fs::write(base.join(".git/HEAD"), "ref: main\n").unwrap();
        std::fs::create_dir_all(base.join("target/debug")).unwrap();
        std::fs::write(base.join("target/debug/app"), "bin\n").unwrap();
        let iso = WriterIsolation::new(&base).unwrap();
        let scope = iso.scope("worker-e").unwrap();
        assert!(!scope.dir.join(".git").exists());
        assert!(!scope.dir.join("target").exists());
        assert!(!scope.dir.join(".susi/dag-scopes").exists());
        // `.susi` worker-visible content still syncs.
        std::fs::create_dir_all(base.join(".susi/evidence")).unwrap();
        std::fs::write(base.join(".susi/evidence/e1.json"), "{}\n").unwrap();
        let scope2 = iso.scope("worker-f").unwrap();
        assert!(scope2.dir.join(".susi/evidence/e1.json").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
