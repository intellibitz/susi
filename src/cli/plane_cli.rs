//! Control-plane prep + exit plumbing shared by the pre-boot dispatch chain
//! (`control_plane_cli`). A "plane" command runs against config/packs/process
//! state directly — it is not a swarm mission.

use std::env;
use std::path::Path;

/// Prep steps before a control-plane CLI handler runs (not a swarm mission).
#[derive(Clone, Copy)]
pub(crate) enum PlanePrep {
    /// No cloud.env / ecosystem priming.
    None,
    /// Apply `~/.susi/cloud.env` only.
    Cloud,
    /// Cloud.env + ensure substrate home exists.
    CloudSubstrate,
    /// Cloud.env + substrate home + catalog priming (`prime_catalogs`).
    CloudEcosystem,
}

pub(crate) fn apply_plane_prep(prep: PlanePrep) {
    match prep {
        PlanePrep::None => {}
        PlanePrep::Cloud => {
            susi_gemi::http_provider::apply_cloud_env_file();
        }
        PlanePrep::CloudSubstrate | PlanePrep::CloudEcosystem => {
            susi_gemi::http_provider::apply_cloud_env_file();
            let substrate = susi_paths::SusiDirs::substrate_home();
            let _ = std::fs::create_dir_all(&substrate);
            if matches!(prep, PlanePrep::CloudEcosystem) {
                let _ = susi_daemon::discovery_pipeline::prime_catalogs(&substrate);
            }
        }
    }
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
