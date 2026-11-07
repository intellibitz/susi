//! Docker execution contract stub: the bollard Docker backend now lives in
//! `susi-commercial-apps` (`susi-fleet-command`).

use susi_error::{EaiError, EaiResult};

/// Returns an error indicating that the Docker sandbox backend lives in `susi-commercial-apps`.
pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
    let _ = cmd;
    Err(EaiError::process(
        "Docker sandbox execution moved to susi-commercial-apps (susi-fleet-command)",
    ))
}
