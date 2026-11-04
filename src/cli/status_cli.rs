//! `susi status`: the CLI side of the status surface. The report itself is
//! `susi_daemon::status_report` (the MCP `operator_status` tool returns the
//! same `--json` document); this module prints it.

use std::path::Path;

use susi_daemon::status_report::{collect, measured_json, render};

/// Print the `[SUBSTRATE IDENTITY]` line that prefixes the human `susi
/// status` report, when the substrate has an identity on disk. It reads one
/// file only, so `main` can answer `status` before substrate boot.
pub(crate) fn print_identity(global_dir: &Path) {
    let id_file = global_dir.join("identity.key");
    if id_file.exists() {
        if let Ok(id) = std::fs::read_to_string(id_file) {
            println!("[SUBSTRATE IDENTITY]: {}", id.trim());
        }
    }
}

/// The `susi status` command body.
pub(crate) fn run_json(cwd: &Path, global_dir: &Path) -> std::process::ExitCode {
    match measured_json(cwd, global_dir) {
        Ok(value) => {
            println!("{value}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("status identity unavailable: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Render the human-readable health snapshot.
pub(crate) fn run(cwd: &Path, global_dir: &Path) {
    let status = collect(
        cwd,
        global_dir,
        &susi_paths::SusiDirs::substrate_home(),
        susi_daemon::status_report::now_unix(),
    );
    print!("{}", render(&status));
}
