//! The `daemon.` plane: capabilities implemented in this composition-root
//! crate, reachable from the MCP tools in `susi-gmcp` (which cannot depend
//! on the daemon) over the in-process plane bus. Each topic calls the same
//! library function its CLI command calls (zero-UI plan B4 groups 1, 3, 4,
//! 5, 7, 8 and 9 and the host views); the facade is
//! `susi_core::plane_bus::daemon`.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use susi_core::plane_bus::{PlaneBus, PlaneHandler, topics};

struct DaemonPlaneHandler;

fn workspace_path(payload: &Value) -> PathBuf {
    payload
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(|v| v.as_str())
}

impl PlaneHandler for DaemonPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        match topic {
            topics::DAEMON_PRIVACY_MODE => {
                let mode = str_field(&payload, "mode").ok_or("mode is required")?;
                crate::privacy::set_privacy_mode(mode).map_err(|e| e.to_string())
            }
            topics::DAEMON_AMBIENT_START => {
                Ok(crate::ambient::start_report(&workspace_path(&payload)))
            }
            topics::DAEMON_CONTEXT_SAMPLE_OS => {
                let ws = workspace_path(&payload);
                Ok(crate::context_adapters::sample_os_report(
                    Some(&ws),
                    str_field(&payload, "user"),
                ))
            }
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
            topics::DAEMON_OS_PROVISION => crate::os_provision::dispatch(&payload),
            topics::DAEMON_SERVICES_CONTROL => {
                let action = str_field(&payload, "action").ok_or("action is required")?;
                let name = str_field(&payload, "name").ok_or("name is required")?;
                crate::service_control::control(action, name)
            }
            topics::DAEMON_PROVIDERS => crate::provider_ops::dispatch(&payload),
            topics::DAEMON_ADAPTERS => crate::adapter_ops::dispatch(&payload),
            topics::DAEMON_HARNESS => crate::harness_ops::dispatch(&payload),
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
    fn handler_refuses_missing_arguments_and_unknown_topics() {
        let h = DaemonPlaneHandler;
        assert!(
            h.handle(topics::DAEMON_PRIVACY_MODE, json!({}))
                .unwrap_err()
                .contains("mode")
        );
        assert!(
            h.handle(topics::DAEMON_SERVICES_CONTROL, json!({"action": "start"}))
                .unwrap_err()
                .contains("name")
        );
        let unknown = h.handle("daemon.nope", json!({})).unwrap();
        assert!(unknown["error"].as_str().unwrap().contains("unhandled"));
    }

    #[test]
    fn provision_and_services_route_to_their_modules() {
        let h = DaemonPlaneHandler;
        assert!(
            h.handle(topics::DAEMON_OS_PROVISION, json!({"action": "explode"}))
                .unwrap_err()
                .contains("unknown provision action")
        );
        assert!(
            h.handle(
                topics::DAEMON_SERVICES_CONTROL,
                json!({"action": "start", "name": "not-a-service"})
            )
            .unwrap_err()
            .contains("unknown service")
        );
    }
}
