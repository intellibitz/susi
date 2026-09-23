//! Tool registration is wired from the composition root (`susi-daemon`).

/// Populated by [`susi_daemon::gmcp_bootstrap::bootstrap_registry`] at startup.
pub fn bootstrap_registry(_registry: &()) {
    // Registration requires `susi-tools::ToolRegistry`; see susi-daemon.
}
