//! Incremental Cell State Snapshotting (Swarm OS Bullet 74)
//!
//! Rather than re-serializing a cell's full memory state on every tick,
//! callers record only the delta since the last snapshot. `reconstruct_state`
//! replays the base plus every recorded delta in order to derive the
//! current state — an append-only redo log, not a binary diff format.
//! Complements `checkpoint.rs`'s full-state, disk-backed snapshots.

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use crate::susi_error::EaiError;

#[derive(Default)]
struct CellHistory {
    base: Vec<u8>,
    diffs: Vec<Vec<u8>>,
}

pub struct SnapshotManager {
    histories: RwLock<HashMap<String, CellHistory>>,
}

impl Default for SnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self {
            histories: RwLock::new(HashMap::new()),
        }
    }

    /// Establishes (or replaces) the full base state for `cell_id`,
    /// discarding any prior incremental history.
    pub fn set_base_snapshot(&self, cell_id: &str, state: Vec<u8>) {
        let mut histories = self.histories.write().unwrap_or_else(|e| e.into_inner());
        histories.insert(
            cell_id.to_string(),
            CellHistory {
                base: state,
                diffs: Vec::new(),
            },
        );
    }

    /// Records an incremental delta on top of the existing history.
    pub fn take_incremental_snapshot(&self, cell_id: &str, diff: &[u8]) {
        let mut histories = self.histories.write().unwrap_or_else(|e| e.into_inner());
        histories
            .entry(cell_id.to_string())
            .or_default()
            .diffs
            .push(diff.to_vec());
    }

    /// Replays the base state plus every recorded delta, in order.
    pub fn reconstruct_state(&self, cell_id: &str) -> Option<Vec<u8>> {
        let histories = self.histories.read().unwrap_or_else(|e| e.into_inner());
        let history = histories.get(cell_id)?;
        let mut state = history.base.clone();
        for diff in &history.diffs {
            state.extend_from_slice(diff);
        }
        Some(state)
    }

    pub fn diff_count(&self, cell_id: &str) -> usize {
        self.histories
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cell_id)
            .map(|h| h.diffs.len())
            .unwrap_or(0)
    }

    /// Exports every cell's reconstructed state as a portable, hex-encoded
    /// JSON bundle (Swarm OS Bullet 40: "export and import memory
    /// snapshots for backup, migration, and offline analysis").
    pub fn export_all(&self, path: &Path) -> Result<(), EaiError> {
        let histories = self.histories.read().unwrap_or_else(|e| e.into_inner());
        let bundle: HashMap<String, String> = histories
            .iter()
            .map(|(cell_id, history)| {
                let mut state = history.base.clone();
                for diff in &history.diffs {
                    state.extend_from_slice(diff);
                }
                (cell_id.clone(), hex::encode(state))
            })
            .collect();
        let payload =
            serde_json::to_vec_pretty(&bundle).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, payload).map_err(|e| EaiError::io(e.to_string()))
    }

    /// Imports a bundle written by [`export_all`](Self::export_all).
    /// Each named cell's base snapshot is replaced by the imported state
    /// (its incremental history starts fresh, same as
    /// [`set_base_snapshot`](Self::set_base_snapshot)). Returns the number
    /// of cells imported.
    pub fn import_all(&self, path: &Path) -> Result<usize, EaiError> {
        let raw = std::fs::read(path).map_err(|e| EaiError::io(e.to_string()))?;
        let bundle: HashMap<String, String> =
            serde_json::from_slice(&raw).map_err(|e| EaiError::io(e.to_string()))?;
        let count = bundle.len();
        for (cell_id, hex_state) in bundle {
            let state = hex::decode(&hex_state).map_err(|e| EaiError::io(e.to_string()))?;
            self.set_base_snapshot(&cell_id, state);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructs_base_plus_ordered_diffs() {
        let mgr = SnapshotManager::new();
        mgr.set_base_snapshot("cell-a", vec![1, 2, 3]);
        mgr.take_incremental_snapshot("cell-a", &[4, 5]);
        mgr.take_incremental_snapshot("cell-a", &[6]);

        assert_eq!(
            mgr.reconstruct_state("cell-a"),
            Some(vec![1, 2, 3, 4, 5, 6])
        );
        assert_eq!(mgr.diff_count("cell-a"), 2);
    }

    #[test]
    fn unknown_cell_has_no_state() {
        let mgr = SnapshotManager::new();
        assert_eq!(mgr.reconstruct_state("ghost"), None);
    }

    #[test]
    fn setting_a_new_base_discards_prior_diffs() {
        let mgr = SnapshotManager::new();
        mgr.set_base_snapshot("cell-a", vec![1]);
        mgr.take_incremental_snapshot("cell-a", &[2]);
        mgr.set_base_snapshot("cell-a", vec![9]);
        assert_eq!(mgr.reconstruct_state("cell-a"), Some(vec![9]));
    }

    #[test]
    fn export_then_import_round_trips_across_managers() {
        let path =
            std::env::temp_dir().join(format!("susi_snapshot_bundle_{}.json", std::process::id()));

        let exporter = SnapshotManager::new();
        exporter.set_base_snapshot("cell-a", vec![1, 2, 3]);
        exporter.take_incremental_snapshot("cell-a", &[4, 5]);
        exporter.set_base_snapshot("cell-b", vec![9]);
        exporter.export_all(&path).unwrap();

        let importer = SnapshotManager::new();
        let count = importer.import_all(&path).unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            importer.reconstruct_state("cell-a"),
            Some(vec![1, 2, 3, 4, 5])
        );
        assert_eq!(importer.reconstruct_state("cell-b"), Some(vec![9]));

        let _ = std::fs::remove_file(&path);
    }
}
