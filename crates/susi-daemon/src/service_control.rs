//! Leaf-service start/stop/restart. Served by `susi-plug-host` on
//! `daemon.services.control`. Public Rust facades keep the original
//! signatures and forward over the plane bus.

use serde_json::Value;
use susi_core::plane_bus::{daemon, topics};

use crate::plug_forward;

/// Apply `action` (`start`, `stop`, or `restart`) to the named leaf service.
pub fn control(action: &str, name: &str) -> Result<Value, String> {
    plug_forward::call(
        daemon::services_control(action, name),
        topics::DAEMON_SERVICES_CONTROL,
    )
}

/// Start the named leaf service under the daemon supervisor.
pub fn start(name: &str) -> Result<Value, String> {
    control("start", name)
}

/// Stop the named leaf service.
pub fn stop(name: &str) -> Result<Value, String> {
    control("stop", name)
}

/// Restart the named leaf service.
pub fn restart(name: &str) -> Result<Value, String> {
    control("restart", name)
}
