//! `MissionHost`: the post-boot context passed to `mission_cli`/`admin_cli`
//! dispatchers, plus the streaming callback that prints a mission's output.

use std::io::{self, Write};
use std::path::Path;

use susi_sandbox::manager::SusiConfig;

/// Post-boot context handed to `mission_cli`/`admin_cli` dispatchers: the
/// live swarm master agent, loaded config, and the two canonical dirs.
/// `susi_gawd::ama::SusiMasterAgent::new()` is the hook-wiring facade — it
/// returns the swarm agent, which is the type held here.
pub(crate) struct MissionHost<'a> {
    pub(crate) cwd: &'a Path,
    pub(crate) global_dir: &'a Path,
    pub(crate) ama: &'a susi_gawd::swarm::ama::SusiMasterAgent,
    pub(crate) cfg: &'a SusiConfig,
}

pub(crate) fn glass_box_callback(piece: String) {
    print!("{}", piece);
    let _ = io::stdout().flush();
}
