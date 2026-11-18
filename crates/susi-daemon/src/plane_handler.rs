//! The `daemon.` plane: capabilities implemented in this composition-root
//! crate, reachable from the MCP tools in `susi-gmcp` (which cannot depend
//! on the daemon) over the in-process plane bus. Each remaining in-process
//! topic calls the same library function its CLI command calls; the facade
//! is `susi_core::plane_bus::daemon`.
//!
//! The first-increment feature topics (`daemon.privacy.mode`,
//! `daemon.ambient.start`, `daemon.services.control`, `daemon.os.provision`,
//! `daemon.context.sample_os`) and the harness operator topic
//! (`daemon.harness`) are served by `susi-plug-host`. Exact topic
//! registrations outrank this prefix, so a running plug host wins. When the
//! host is absent this prefix names that command instead of handling them
//! in-process.

use serde_json::{Value, json};
use std::sync::Arc;
use susi_core::plane_bus::{PlaneBus, PlaneHandler, topics, workspace_path};

use crate::plug_forward;

struct DaemonPlaneHandler;

impl PlaneHandler for DaemonPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        match topic {
            topics::DAEMON_PRIVACY_MODE
            | topics::DAEMON_AMBIENT_START
            | topics::DAEMON_CONTEXT_SAMPLE_OS
            | topics::DAEMON_OS_PROVISION
            | topics::DAEMON_SERVICES_CONTROL
            | topics::DAEMON_HARNESS => Err(plug_forward::absent(topic)),
            topics::DAEMON_ECOSYSTEM_SCAN => {
                if payload.get("cached").and_then(|v| v.as_bool()) == Some(true) {
                    let text = crate::discovery_pipeline::last_ecosystem_scan()?;
                    serde_json::from_str(&text)
                        .map_err(|e| format!("last ecosystem scan is not JSON: {e}"))
                } else {
                    Ok(crate::discovery_pipeline::local_ecosystem_scan_report())
                }
            }
            topics::DAEMON_SUBSTRATE_STATUS => {
                Ok(crate::substrate_status::report(&workspace_path(&payload)))
            }
            topics::DAEMON_CROWN_VERIFY => Ok(crate::crown::report(&workspace_path(&payload))),
            topics::DAEMON_PROVIDERS => crate::provider_ops::dispatch(&payload),
            topics::DAEMON_ADAPTERS => crate::adapter_ops::dispatch(&payload),
            topics::DAEMON_HOST => crate::host_views::dispatch(&payload),
            other => Ok(json!({ "error": format!("daemon handler: unhandled topic '{other}'") })),
        }
    }
}

/// Register the `daemon.` prefix on the process plane bus.
pub fn register() {
    PlaneBus::global().register_prefix("daemon.", Arc::new(DaemonPlaneHandler));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_feature_topics_name_the_plug_host_when_this_prefix_handles_them() {
        let h = DaemonPlaneHandler;
        for topic in [
            topics::DAEMON_PRIVACY_MODE,
            topics::DAEMON_AMBIENT_START,
            topics::DAEMON_CONTEXT_SAMPLE_OS,
            topics::DAEMON_OS_PROVISION,
            topics::DAEMON_SERVICES_CONTROL,
            topics::DAEMON_HARNESS,
        ] {
            let err = h.handle(topic, json!({})).unwrap_err();
            assert!(err.contains("susi-plug-host"), "{topic}: {err}");
        }
        let unknown = h.handle("daemon.nope", json!({})).unwrap();
        assert!(unknown["error"].as_str().unwrap().contains("unhandled"));
    }

    #[test]
    fn substrate_status_returns_a_structured_payload() {
        let h = DaemonPlaneHandler;
        let status = h
            .handle(topics::DAEMON_SUBSTRATE_STATUS, json!({"workspace": "."}))
            .expect("substrate status should succeed");
        assert!(status.is_object());
    }

    #[test]
    fn register_installs_daemon_prefix_on_global_plane_bus() {
        register();
        let resp = PlaneBus::global()
            .request("daemon.unrecognized_topic_for_test", json!({}))
            .expect("registered daemon prefix should handle call");
        assert!(
            resp["error"]
                .as_str()
                .unwrap_or_default()
                .contains("unhandled topic")
        );
    }

    #[test]
    fn handler_routes_providers_validation() {
        let h = DaemonPlaneHandler;
        assert!(
            h.handle(topics::DAEMON_PROVIDERS, json!({}))
                .unwrap_err()
                .contains("action")
        );
    }
}
