//! Control-plane prep + exit plumbing shared by the pre-boot dispatch chain
//! (`control_plane_cli`). A "plane" command runs against config/packs/process
//! state directly — it is not a swarm mission.

use std::env;
use std::path::Path;
use susi_daemon::discovery_pipeline::Catalogs;

/// Prep steps before a control-plane CLI handler runs (not a swarm mission).
#[derive(Clone, Copy)]
pub(crate) enum PlanePrep {
    /// No priming.
    None,
    /// Ensure the substrate home exists.
    Substrate,
    /// Substrate home + priming of exactly these catalogs
    /// (`prime_selected_catalogs`); see `defs::command_catalogs`.
    Ecosystem(Catalogs),
}

pub(crate) fn apply_plane_prep(prep: PlanePrep) {
    match prep {
        PlanePrep::None => {}
        PlanePrep::Substrate | PlanePrep::Ecosystem(_) => {
            let substrate = susi_paths::SusiDirs::substrate_home();
            let _ = std::fs::create_dir_all(&substrate);
            if let PlanePrep::Ecosystem(catalogs) = prep {
                prime_plane_catalogs(&substrate, catalogs);
            }
        }
    }
}

/// Prime `catalogs` for a control-plane command. A command that reads no
/// catalog primes nothing, but still gets the extension pack seeded, as
/// every prime always did.
fn prime_plane_catalogs(substrate: &Path, catalogs: Catalogs) {
    if catalogs.is_empty() {
        let _ = susi_sandbox::extensions::ensure_extensions_substrate();
        return;
    }
    let _ = susi_daemon::boot_trace::phase("plane: prime catalogs", || {
        susi_daemon::discovery_pipeline::prime_selected_catalogs(substrate, catalogs)
    });
}

pub(crate) fn plane_exit(result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", susi_agents::external::redact(&e.to_string()));
            std::process::ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_plane_cwd(
    prep: PlanePrep,
    f: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> std::process::ExitCode {
    apply_plane_prep(prep);
    plane_exit(
        env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| f(&cwd)),
    )
}

pub(crate) fn run_plane(
    prep: PlanePrep,
    f: impl FnOnce() -> anyhow::Result<()>,
) -> std::process::ExitCode {
    apply_plane_prep(prep);
    plane_exit(f())
}
