//! Swarm Cell spawning for files dropped into `~/.susi/cells/`.
//!
//! Only the daemon spawns cells: once at boot ([`spawn_all_cells`]) and then
//! per newly added file from the cell watcher ([`spawn_cell`]). Catalog and
//! capability priming lives in [`crate::discovery_pipeline`].

use std::path::Path;

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
            let _ = std::process::Command::new(path)
                .env(susi_abi::syscall::CELL_TOKEN_ENV, cell_token())
                .spawn();
        });
    } else if name.ends_with(".json") {
        static PORT_COUNTER: std::sync::atomic::AtomicU16 =
            std::sync::atomic::AtomicU16::new(10000);
        let port = PORT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bind_addr = format!("127.0.0.1:{port}");
        // Sibling of the running binary; bare name (PATH lookup) otherwise.
        let universal_cell_path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("susi-universal-cell")))
            .unwrap_or_else(|| "susi-universal-cell".into());
        std::thread::spawn(move || {
            let _ = std::process::Command::new(universal_cell_path)
                .env(susi_abi::syscall::CELL_TOKEN_ENV, cell_token())
                .arg(bind_addr)
                .arg(path)
                .spawn();
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
}
