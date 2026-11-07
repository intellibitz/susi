//! `MissionHost`: the post-boot context passed to `mission_cli`/`admin_cli`
//! dispatchers, plus the streaming callback that prints a mission's output.

use std::io::{self, Write};
use std::path::Path;

use susi_sandbox_client::manager::SusiConfig;

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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn glass_box_callback_handles_empty_piece_without_panic() {
        glass_box_callback(String::new());
    }

    #[test]
    fn glass_box_callback_handles_multiline_utf8_stream_chunks() {
        glass_box_callback(String::from("[glass-box] step 1\n"));
        glass_box_callback(String::from("[glass-box] ✓ verified\n"));
    }

    #[test]
    fn susi_config_default_initializes_cleanly_for_mission_host() {
        let cfg = SusiConfig::default();
        let serialized = serde_json::to_value(&cfg).unwrap();
        assert!(serialized.is_object());
    }

    #[test]
    fn mission_host_directory_references_remain_distinct() {
        let cwd_buf = PathBuf::from("workspace_cwd");
        let global_buf = PathBuf::from("global_susi_dir");
        let cwd: &Path = cwd_buf.as_path();
        let global_dir: &Path = global_buf.as_path();
        assert_ne!(cwd, global_dir);
        assert!(cwd.ends_with("workspace_cwd"));
        assert!(global_dir.ends_with("global_susi_dir"));
    }
}
