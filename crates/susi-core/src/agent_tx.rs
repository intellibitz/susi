//! Multi-agent transactional snapshots with compensating rollback.
//!
//! Captures workspace file contents and blackboard entries at begin, applies
//! participating changes, and on abort restores both — a saga-style boundary
//! for mission workflows (not a full ACID DB).

use crate::susi_error::{EaiError, EaiResult};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TxStatus {
    Open,
    Committed,
    Aborted,
}

/// One file version captured at snapshot time (`None` = did not exist).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub rel_path: String,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTransaction {
    pub id: String,
    pub workspace: String,
    pub description: String,
    pub status: TxStatus,
    pub files: Vec<FileSnapshot>,
    pub blackboard: BTreeMap<String, String>,
    pub created_at: u64,
    pub closed_at: Option<u64>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn tx_dir(workspace: &Path) -> PathBuf {
    workspace.join(".susi").join("tx")
}

/// A transaction closes only against the workspace it snapshotted. Ids are
/// unique per workspace journal, not globally, so a caller naming the
/// wrong workspace is refused rather than acted on.
fn ensure_same_workspace(tx: &AgentTransaction, workspace: &Path) -> EaiResult<()> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if canonical(Path::new(&tx.workspace)) == canonical(workspace) {
        return Ok(());
    }
    Err(EaiError::governance(format!(
        "tx {} belongs to workspace {}, not {}",
        tx.id,
        tx.workspace,
        workspace.display()
    )))
}

fn confined(workspace: &Path, rel: &str) -> EaiResult<PathBuf> {
    let rel = rel.replace('\\', "/").trim_start_matches('/').to_string();
    if rel.is_empty() || rel.contains("..") {
        return Err(EaiError::governance(format!("tx path escape: {rel}")));
    }
    // Resolve through the deepest existing ancestor: files a transaction
    // creates under a symlinked dir must not escape the workspace (the old
    // check waved through every path that did not exist yet).
    crate::susi_config::confined_workspace_join(workspace, &rel)
        .map_err(|e| EaiError::governance(format!("tx path outside workspace: {rel} ({e})")))
}

/// Process-wide transaction registry.
pub struct TxManager {
    open: DashMap<String, AgentTransaction>,
    next_id: AtomicU64,
}

impl TxManager {
    pub fn new() -> Self {
        Self {
            open: DashMap::new(),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn global() -> &'static Self {
        static M: OnceLock<TxManager> = OnceLock::new();
        M.get_or_init(TxManager::new)
    }

    /// Begin a transaction: snapshot listed relative files + optional blackboard map.
    pub fn begin(
        &self,
        workspace: &Path,
        description: &str,
        file_rels: &[String],
        blackboard: BTreeMap<String, String>,
    ) -> EaiResult<AgentTransaction> {
        let mut files = Vec::new();
        for rel in file_rels {
            let path = confined(workspace, rel)?;
            let content = if path.is_file() {
                Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| EaiError::filesystem(format!("snapshot {rel}: {e}")))?,
                )
            } else {
                None
            };
            files.push(FileSnapshot {
                rel_path: rel.clone(),
                content,
            });
        }
        let id = self.reserve_id(workspace)?;
        let tx = AgentTransaction {
            id: id.clone(),
            workspace: workspace.display().to_string(),
            description: description.into(),
            status: TxStatus::Open,
            files,
            blackboard,
            created_at: now(),
            closed_at: None,
        };
        // The durable copy is what lets another process (a later CLI
        // call) abort this transaction; a begin that could not write it
        // must not report success.
        self.persist(&tx)?;
        self.open.insert(id, tx.clone());
        Ok(tx)
    }

    /// Mint an id no other transaction in this workspace has used, by
    /// claiming `.susi/tx/<id>.json` with `create_new`. The counter is
    /// per-process, so a process that never hydrated used to mint `tx-1`
    /// again and overwrite an earlier transaction's rollback snapshot.
    fn reserve_id(&self, workspace: &Path) -> EaiResult<String> {
        let dir = tx_dir(workspace);
        std::fs::create_dir_all(&dir)
            .map_err(|e| EaiError::filesystem(format!("create {}: {e}", dir.display())))?;
        for _ in 0..10_000 {
            let id = format!("tx-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(format!("{id}.json")))
            {
                Ok(_) => return Ok(id),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(EaiError::filesystem(format!("reserve {id}: {e}")));
                }
            }
        }
        Err(EaiError::filesystem("no free transaction id"))
    }

    pub fn commit(&self, tx_id: &str, workspace: &Path) -> EaiResult<AgentTransaction> {
        let mut entry = self
            .open
            .get_mut(tx_id)
            .ok_or_else(|| EaiError::governance(format!("unknown tx {tx_id}")))?;
        if entry.status != TxStatus::Open {
            return Err(EaiError::governance("transaction not open"));
        }
        ensure_same_workspace(&entry, workspace)?;
        entry.status = TxStatus::Committed;
        entry.closed_at = Some(now());
        let out = entry.clone();
        drop(entry);
        self.open.remove(tx_id);
        // A commit that stays Open on disk would let a later process's
        // abort roll back committed work.
        self.persist(&out)?;
        Ok(out)
    }

    /// Abort: restore snapshotted files. Returns blackboard map to restore by caller.
    pub fn abort(&self, tx_id: &str, workspace: &Path) -> EaiResult<AgentTransaction> {
        let mut entry = self
            .open
            .get_mut(tx_id)
            .ok_or_else(|| EaiError::governance(format!("unknown tx {tx_id}")))?;
        if entry.status != TxStatus::Open {
            return Err(EaiError::governance("transaction not open"));
        }
        // Snapshots are relative paths: restoring them under another
        // workspace would write one project's contents into another.
        ensure_same_workspace(&entry, workspace)?;
        for snap in &entry.files {
            let path = confined(workspace, &snap.rel_path)?;
            match &snap.content {
                Some(content) => {
                    crate::susi_config::atomic_replace_file(&path, content.as_ref()).map_err(
                        |e| EaiError::filesystem(format!("restore {}: {e}", snap.rel_path)),
                    )?;
                }
                None => {
                    if path.exists() {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        entry.status = TxStatus::Aborted;
        entry.closed_at = Some(now());
        let out = entry.clone();
        drop(entry);
        self.open.remove(tx_id);
        self.persist(&out)?;
        Ok(out)
    }

    /// Re-load persisted transactions from `workspace/.susi/tx/` into the
    /// in-memory map. Each CLI invocation is a fresh process — without
    /// hydration `list`, `commit` and `abort` can never see a `begin`
    /// issued by an earlier call. Closed rows on disk also shadow stale
    /// in-memory open rows for the same id.
    pub fn hydrate(&self, workspace: &Path) {
        let dir = workspace.join(".susi").join("tx");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut max_id = 0u64;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(tx) = serde_json::from_str::<AgentTransaction>(&text) else {
                continue;
            };
            if let Some(n) = tx
                .id
                .strip_prefix("tx-")
                .and_then(|s| s.parse::<u64>().ok())
            {
                max_id = max_id.max(n + 1);
            }
            if tx.status == TxStatus::Open {
                self.open.insert(tx.id.clone(), tx);
            } else {
                self.open.remove(&tx.id);
            }
        }
        // A hydrated tx-7 must not collide with the next `begin` minting
        // tx-1 in a fresh process — advance the counter past disk state.
        self.next_id.fetch_max(max_id, Ordering::Relaxed);
    }

    /// Replace the durable copy whole: a torn write is skipped by
    /// `hydrate`, silently losing an open transaction's snapshot.
    fn persist(&self, tx: &AgentTransaction) -> EaiResult<()> {
        let path = tx_dir(Path::new(&tx.workspace)).join(format!("{}.json", tx.id));
        let body = serde_json::to_vec_pretty(tx)?;
        crate::susi_config::atomic_write_bytes(&path, &body)
            .map_err(|e| EaiError::filesystem(format!("persist {}: {e}", path.display())))
    }

    pub fn get(&self, tx_id: &str) -> Option<AgentTransaction> {
        self.open.get(tx_id).map(|e| e.value().clone())
    }

    pub fn list_open(&self) -> Vec<AgentTransaction> {
        self.open.iter().map(|e| e.value().clone()).collect()
    }
}

impl Default for TxManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws() -> PathBuf {
        std::env::temp_dir().join(format!(
            "susi-tx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn abort_restores_files() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("a.txt"), "original").unwrap();
        let mgr = TxManager::new();
        let tx = mgr
            .begin(&ws, "edit a", &["a.txt".into()], BTreeMap::new())
            .unwrap();
        std::fs::write(ws.join("a.txt"), "mutated").unwrap();
        let aborted = mgr.abort(&tx.id, &ws).unwrap();
        assert_eq!(aborted.status, TxStatus::Aborted);
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).unwrap(),
            "original"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_fresh_process_never_reuses_a_persisted_id() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("c.txt"), "v1").unwrap();
        let first = TxManager::new()
            .begin(&ws, "first", &["c.txt".into()], BTreeMap::new())
            .unwrap();
        // A second process that never hydrated starts its counter at 1.
        let second = TxManager::new()
            .begin(&ws, "second", &["c.txt".into()], BTreeMap::new())
            .unwrap();
        assert_ne!(first.id, second.id);
        let on_disk: AgentTransaction = serde_json::from_str(
            &std::fs::read_to_string(tx_dir(&ws).join(format!("{}.json", first.id))).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk.description, "first");

        // The first process's snapshot is still recoverable elsewhere.
        std::fs::write(ws.join("c.txt"), "mutated").unwrap();
        let recovering = TxManager::new();
        recovering.hydrate(&ws);
        recovering.abort(&first.id, &ws).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("c.txt")).unwrap(), "v1");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn closing_from_another_workspace_is_refused() {
        let ws = temp_ws();
        let other = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        let _ = std::fs::create_dir_all(&other);
        std::fs::write(ws.join("d.txt"), "mine").unwrap();
        std::fs::write(other.join("d.txt"), "theirs").unwrap();
        let mgr = TxManager::new();
        let tx = mgr
            .begin(&ws, "d", &["d.txt".into()], BTreeMap::new())
            .unwrap();
        assert!(mgr.abort(&tx.id, &other).is_err());
        assert!(mgr.commit(&tx.id, &other).is_err());
        assert_eq!(
            std::fs::read_to_string(other.join("d.txt")).unwrap(),
            "theirs"
        );
        mgr.abort(&tx.id, &ws).unwrap();
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn commit_keeps_mutations() {
        let ws = temp_ws();
        let _ = std::fs::create_dir_all(&ws);
        std::fs::write(ws.join("b.txt"), "v1").unwrap();
        let mgr = TxManager::new();
        let tx = mgr
            .begin(&ws, "edit b", &["b.txt".into()], BTreeMap::new())
            .unwrap();
        std::fs::write(ws.join("b.txt"), "v2").unwrap();
        mgr.commit(&tx.id, &ws).unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("b.txt")).unwrap(), "v2");
        let _ = std::fs::remove_dir_all(&ws);
    }
}
