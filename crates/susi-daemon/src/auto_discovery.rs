//! Swarm Cell spawning for files dropped into `~/.susi/cells/`.
//!
//! Only the daemon spawns cells: once at boot ([`spawn_all_cells`]) and then
//! per newly added file from the cell watcher ([`spawn_cell`]). Catalog and
//! capability priming lives in [`crate::discovery_pipeline`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Mutex, OnceLock};

/// Child processes of spawned process cells, keyed by their cell file.
fn running_cells() -> &'static Mutex<HashMap<PathBuf, Child>> {
    static CELLS: OnceLock<Mutex<HashMap<PathBuf, Child>>> = OnceLock::new();
    CELLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn track(path: PathBuf, spawned: std::io::Result<Child>) {
    match spawned {
        Ok(child) => {
            let mut cells = running_cells().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut previous) = cells.insert(path, child) {
                // A re-added file replaces its old process.
                let _ = previous.kill();
                let _ = previous.wait();
            }
        }
        Err(e) => tracing::warn!(
            "[auto_discovery] Failed to spawn cell {}: {e}",
            path.display()
        ),
    }
}

/// Reap tracked cells whose process already exited (so they do not linger
/// as zombies) and return their files; the watcher respawns them only when
/// their file is re-added.
pub fn reap_exited_cells() -> Vec<PathBuf> {
    let mut cells = running_cells().lock().unwrap_or_else(|e| e.into_inner());
    let exited: Vec<PathBuf> = cells
        .iter_mut()
        .filter_map(|(path, child)| matches!(child.try_wait(), Ok(Some(_))).then(|| path.clone()))
        .collect();
    for path in &exited {
        cells.remove(path);
    }
    exited
}

/// Stop every tracked process cell (daemon shutdown). Returns how many
/// were terminated.
pub fn stop_all_cells() -> usize {
    let cells: Vec<Child> = running_cells()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .map(|(_, child)| child)
        .collect();
    let stopped = cells.len();
    for mut child in cells {
        let _ = child.kill();
        let _ = child.wait();
    }
    stopped
}

/// Stop the process cell spawned for `path` (its file was removed).
/// Returns `true` when a running process was found and terminated.
/// In-process WASM cells run to completion and are not tracked.
pub fn stop_cell(path: &Path) -> bool {
    let child = running_cells()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(path);
    match child {
        Some(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            true
        }
        None => false,
    }
}

/// Token spawned Swarm Cells require on every syscall: the host API token.
fn cell_token() -> String {
    crate::susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded()
}

/// Spawn every cell currently in `<substrate>/cells`, creating the directory
/// when missing. Call once per daemon lifetime; later additions go through
/// [`spawn_cell`].
pub fn spawn_all_cells(substrate: &Path) {
    let cells_dir = substrate.join("cells");
    if !cells_dir.exists() {
        let _ = std::fs::create_dir_all(&cells_dir);
        return;
    }
    if let Ok(entries) = std::fs::read_dir(&cells_dir) {
        for entry in entries.flatten() {
            spawn_cell(&entry.path());
        }
    }
}

/// Spawn a single cell file. Returns `false` for files that are not cells.
///
/// * `susi-cell-*` / `*.cell` — executable cell, run directly
/// * `*.json` — plugin manifest, hosted by `susi-universal-cell`
/// * `*.wasm` — loaded in-process via [`crate::sandbox_wasm`]
pub fn spawn_cell(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let path = path.to_path_buf();
    if name.starts_with("susi-cell-") || name.ends_with(".cell") {
        std::thread::spawn(move || {
            let spawned = std::process::Command::new(&path)
                .env(crate::susi_abi::syscall::CELL_TOKEN_ENV, cell_token())
                .spawn();
            track(path, spawned);
        });
    } else if name.ends_with(".json") {
        // Let the OS pick a free loopback port; a fixed counter handed out
        // ports without checking them and collided with anything already
        // listening there.
        let Some(bind_addr) = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .ok()
            .map(|a| a.to_string())
        else {
            tracing::warn!(
                "[auto_discovery] No free loopback port for {}",
                path.display()
            );
            return false;
        };
        // Sibling of the running binary; bare name (PATH lookup) otherwise.
        let universal_cell_path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("susi-universal-cell")))
            .unwrap_or_else(|| "susi-universal-cell".into());
        std::thread::spawn(move || {
            let spawned = std::process::Command::new(universal_cell_path)
                .env(crate::susi_abi::syscall::CELL_TOKEN_ENV, cell_token())
                .arg(bind_addr)
                .arg(&path)
                .spawn();
            track(path, spawned);
        });
    } else if name.ends_with(".wasm") {
        // Load the cell in-process and run its `_start` entry point.
        std::thread::spawn(
            move || match crate::sandbox_wasm::spawn_wasm_cell(path.clone()) {
                Ok(mut cell) => {
                    tracing::info!("[auto_discovery] Loaded WASM cell: {}", path.display());
                    if let Err(e) = cell.execute("_start") {
                        tracing::warn!("[auto_discovery] WASM cell _start failed: {:#}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[auto_discovery] Failed to load WASM cell {}: {:#}",
                        path.display(),
                        e
                    );
                }
            },
        );
    } else {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_cell_files_are_not_spawned() {
        let dir = std::env::temp_dir().join(format!("susi_cells_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let readme = dir.join("README.md");
        std::fs::write(&readme, "not a cell").unwrap();
        assert!(!spawn_cell(&readme));
        assert!(!spawn_cell(&dir)); // directories are never cells
        assert!(!spawn_cell(&dir.join("missing.cell")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn removed_process_cells_are_stopped() {
        let cell = std::path::PathBuf::from(format!("/tmp/susi-cell-stop-{}", std::process::id()));
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        track(cell.clone(), Ok(child));
        assert!(stop_cell(&cell), "tracked cell must be stopped");
        assert!(!stop_cell(&cell), "already stopped");
        // Killed and reaped: the pid is gone.
        let alive = std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(!alive, "cell process {pid} still running");
    }

    #[cfg(unix)]
    #[test]
    fn exited_cells_are_reaped() {
        let cell = std::path::PathBuf::from(format!("/tmp/susi-cell-reap-{}", std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let _ = child.try_wait();
        std::thread::sleep(std::time::Duration::from_millis(200));
        track(cell.clone(), Ok(child));
        assert!(reap_exited_cells().contains(&cell));
        assert!(!stop_cell(&cell), "reaped cell is no longer tracked");
    }
}
