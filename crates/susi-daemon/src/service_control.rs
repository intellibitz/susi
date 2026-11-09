//! Operator control of supervised leaf services: what `susi services
//! start|stop|restart` (and `susi os manage service ..`) do, and what the
//! MCP `services_start`, `services_stop` and `services_restart` tools do
//! over the `daemon.services.control` plane topic (zero-UI plan B4 group 9).
//!
//! Each call returns `{service, action, pid, changed, message}`; `message`
//! is the line the CLI prints. Termination uses the platform's own process
//! tool (`kill -TERM` on Unix, `taskkill /T /F` on Windows), so restart and
//! stop work on every supported OS.

use serde_json::{Value, json};
use susi_core::service_table;

fn daemon_running() -> bool {
    crate::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir()).is_some()
}

/// Terminate a supervised pid: SIGTERM on Unix, `taskkill /T /F` on
/// Windows (the analogue of SIGTERM on a supervised child).
fn terminate(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    let ok = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    #[cfg(windows)]
    let ok = susi_paths::console::background_command("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    #[cfg(not(any(unix, windows)))]
    let ok = false;
    if ok {
        Ok(())
    } else {
        Err(format!("failed to terminate pid {pid}"))
    }
}

fn reply(name: &str, action: &str, pid: Option<u32>, changed: bool, message: String) -> Value {
    json!({
        "service": name,
        "action": action,
        "pid": pid,
        "changed": changed,
        "message": message,
    })
}

/// Terminate a supervised service; the daemon respawns it on its next
/// probe.
pub fn restart(name: &str) -> Result<Value, String> {
    let Some(svc) = service_table::leaf_service(name) else {
        return Err(format!(
            "unknown service `{name}` (expected one of the leaf services)"
        ));
    };
    let table = service_table::load();
    let Some(rec) = table.iter().find(|r| r.name == svc.name) else {
        return Err(format!(
            "{name} is not supervised by the daemon — nothing to restart \
             (start the daemon with `susi start` to put it under supervision)"
        ));
    };
    if rec.external {
        // The "never kill what we didn't spawn" rule applies to operator
        // restarts too — an external process is not ours to signal.
        return Err(format!(
            "{name} is bound by an external process (pid {}) that the daemon \
             does not supervise — stop it yourself and the supervisor will \
             spawn a managed service on the freed port",
            rec.pid
        ));
    }
    if !daemon_running() {
        return Err(format!(
            "the daemon is not running — killing {name} (pid {}) would leave it \
             dead with nothing to respawn it; start the daemon with `susi start` first",
            rec.pid
        ));
    }
    terminate(rec.pid).map_err(|e| format!("could not restart {name}: {e}"))?;
    Ok(reply(
        name,
        "restart",
        Some(rec.pid),
        true,
        format!(
            "terminated {name} (pid {}); the daemon will respawn it",
            rec.pid
        ),
    ))
}

/// Terminate a supervised pid AND mark the row stopped so the monitor
/// leaves it down. Same guards as restart: supervised-only (external rows
/// are not ours to signal) and the daemon must be running — the stop flag
/// is meaningless if nothing honors it.
pub fn stop(name: &str) -> Result<Value, String> {
    let Some(svc) = service_table::leaf_service(name) else {
        return Err(format!(
            "unknown service `{name}` (expected one of the leaf services)"
        ));
    };
    if !daemon_running() {
        return Err(format!(
            "the daemon is not running — {name} is already unsupervised; \
             kill its pid directly if it is still alive"
        ));
    }
    // Set the flag BEFORE signaling — otherwise the monitor can win the
    // race: see the dead pid, respawn a fresh child, then have its table
    // overwritten by our flag with the new child orphaned. update_with
    // serializes the load→mutate→save against the monitor's own pass.
    let pid = service_table::update_with(|table| -> Result<u32, String> {
        let Some(rec) = table.iter_mut().find(|r| r.name == svc.name) else {
            return Err(format!(
                "{name} is not in the process table — nothing to stop"
            ));
        };
        if rec.external {
            return Err(format!(
                "{name} is bound by an external process (pid {}) that the daemon \
                 does not supervise — stop it yourself",
                rec.pid
            ));
        }
        rec.stopped = true;
        Ok(rec.pid)
    })
    .map_err(|e| e.to_string())??;
    // A pid that already died needs no signal on Unix; Windows taskkill
    // reports the dead pid as a failure, which is still a stop.
    #[cfg(unix)]
    if service_table::pid_alive(pid) {
        terminate(pid).map_err(|e| format!("{name} marked stopped but {e}"))?;
    }
    #[cfg(not(unix))]
    terminate(pid).map_err(|e| format!("{name} marked stopped but {e}"))?;
    Ok(reply(
        name,
        "stop",
        Some(pid),
        true,
        format!(
            "stopped {name} (pid {pid}); supervisor holds it down until `susi services start {name}`"
        ),
    ))
}

/// Clear the operator stop flag — the monitor respawns the service on its
/// next pass (fresh restart budget, cooldown cleared).
pub fn start(name: &str) -> Result<Value, String> {
    let Some(svc) = service_table::leaf_service(name) else {
        return Err(format!(
            "unknown service `{name}` (expected one of the leaf services)"
        ));
    };
    // Same serialization as stop — a plain load→save here can be
    // clobbered by the monitor's pass, silently losing the start.
    let cleared = service_table::update_with(|table| {
        table.iter_mut().find(|r| r.name == svc.name).map(|rec| {
            if !rec.stopped {
                return false;
            }
            rec.stopped = false;
            rec.disabled_until = None;
            true
        })
    })
    .map_err(|e| e.to_string())?;
    Ok(match cleared {
        Some(true) => reply(
            name,
            "start",
            None,
            true,
            format!("cleared stop on {name}; the supervisor will respawn it within ~5s"),
        ),
        Some(false) => reply(name, "start", None, false, format!("{name} is not stopped")),
        // No record at all — dropping the name into the table as
        // stopped=false adds nothing; the monitor's missing-service path
        // already retries unsupervised services on a 30s cadence. A
        // record-less `start` is a no-op the operator should hear about
        // honestly.
        None => reply(
            name,
            "start",
            None,
            false,
            format!(
                "{name} has no process-table row — the supervisor retries missing \
                 services on its own; nothing was changed"
            ),
        ),
    })
}

/// Dispatch one `daemon.services.control` request.
pub fn control(action: &str, name: &str) -> Result<Value, String> {
    match action {
        "start" => start(name),
        "stop" => stop(name),
        "restart" => restart(name),
        other => Err(format!(
            "unknown services action `{other}` (expected start, stop or restart)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_services_and_actions_are_refused() {
        assert!(
            control("start", "not-a-service")
                .unwrap_err()
                .contains("unknown service")
        );
        assert!(control("stop", "not-a-service").is_err());
        assert!(control("restart", "not-a-service").is_err());
        assert!(
            control("explode", "susi-paths")
                .unwrap_err()
                .contains("unknown services action")
        );
    }

    #[test]
    fn reply_builds_structured_service_action_payload() {
        let with_pid = reply("gmcp", "restart", Some(4242), true, "restarted".into());
        assert_eq!(with_pid["service"], "gmcp");
        assert_eq!(with_pid["action"], "restart");
        assert_eq!(with_pid["pid"], 4242);
        assert_eq!(with_pid["changed"], true);
        assert_eq!(with_pid["message"], "restarted");

        let without_pid = reply("gemi", "start", None, false, "noop".into());
        assert!(without_pid["pid"].is_null());
        assert_eq!(without_pid["changed"], false);
    }

    #[test]
    fn empty_service_name_is_refused_across_all_actions() {
        for action in ["start", "stop", "restart"] {
            let err = control(action, "").expect_err("empty service name must fail");
            assert!(err.contains("unknown service"), "action={action} err={err}");
        }
    }

    #[test]
    fn control_refuses_blank_action_and_validates_action_before_service() {
        let err = control("", "not-a-service").expect_err("blank action must fail");
        assert!(err.contains("unknown services action"), "err={err}");
    }

    #[test]
    fn direct_start_stop_and_restart_functions_reject_unknown_service_names() {
        assert!(
            start("nonexistent-svc")
                .unwrap_err()
                .contains("unknown service")
        );
        assert!(
            stop("nonexistent-svc")
                .unwrap_err()
                .contains("unknown service")
        );
        assert!(
            restart("nonexistent-svc")
                .unwrap_err()
                .contains("unknown service")
        );
    }
}
