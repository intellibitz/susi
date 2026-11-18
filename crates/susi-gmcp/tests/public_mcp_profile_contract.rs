//! Exercise the supported public Rust paths from outside the GMCP crate.
use susi_gmcp::mcp_probe::{
    McpProbeError, McpProbeNotification, McpProbeOptions, McpProbeRequest, McpProbeResponse,
    McpProbeTransport,
};
use susi_gmcp::mcp_profile::McpSpecProfile;

#[derive(Default)]
struct OfflineTransport {
    requests: Vec<String>,
}

impl McpProbeTransport for OfflineTransport {
    fn request(&mut self, request: &McpProbeRequest) -> Result<McpProbeResponse, McpProbeError> {
        self.requests.push(request.method.clone());
        Err(McpProbeError::Transport {
            method: request.method.clone(),
            detail: "peer is offline".to_owned(),
        })
    }

    fn probe_malformed_response(
        &mut self,
        _timeout: std::time::Duration,
        _protocol_version: &str,
    ) -> Result<(), McpProbeError> {
        Err(McpProbeError::Transport {
            method: "probe_malformed_response".into(),
            detail: "peer is offline".into(),
        })
    }

    fn probe_credential_isolation(&self) -> Result<(), McpProbeError> {
        // This transport only records method names and never opens a socket.
        Ok(())
    }

    fn notify(&mut self, _notification: &McpProbeNotification) -> Result<(), McpProbeError> {
        Ok(())
    }
}

#[test]
fn downstream_profile_negotiates_and_probes_a_caller_owned_transport() {
    let mut profile = McpSpecProfile::default_profile();
    assert_eq!(profile.registry_metadata["probe_status"], "unprobed");
    assert_eq!(
        profile.negotiate(&["2025-03-26"]).as_deref(),
        Some("2025-03-26")
    );
    let mut peer = OfflineTransport::default();
    let report = profile.probe(&mut peer, McpProbeOptions::default());
    assert_eq!(
        peer.requests.first().map(String::as_str),
        Some("initialize")
    );
    assert!(!report.passed());
    assert_eq!(profile.registry_metadata["probe_status"], "failed");
}
