//! Zero-config ecosystem discovery.
//!
//! Delegated to Swarm OS cells (`susi-gawd` / `susi-gmcp`) over IPC.

use std::path::Path;

/// Orchestrate dynamic capabilities on engine boot.
pub fn auto_prime_ecosystem(substrate: &Path) {
    let cells_dir = substrate.join("cells");
    if !cells_dir.exists() {
        let _ = std::fs::create_dir_all(&cells_dir);
        return;
    }

    if let Ok(entries) = std::fs::read_dir(&cells_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                // If the file is executable (or just a file in the cells dir), spawn it as a Swarm Cell
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    #[allow(clippy::collapsible_if)]
                    if name.starts_with("susi-cell-") || name.ends_with(".cell") {
                        std::thread::spawn(move || {
                            let _ = std::process::Command::new(path).spawn();
                        });
                    }
                }
            }
        }
    }
}

pub fn bootstrap_zero_config_substrate(_substrate: &Path) {
    // Delegated to Swarm OS.
}
