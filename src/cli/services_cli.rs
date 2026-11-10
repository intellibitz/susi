//! `susi services` — the process-table view of the microkernel's leaf
//! services: which are supervised by the daemon, which ports are live.
//! `restart` SIGTERMs a supervised pid; the daemon's supervisor notices
//! the death on its next probe and respawns it.

use crate::anyhow::{bail, Result};
use std::path::Path;
use susi_core::service_table;

pub use susi_cli_defs::services::*;

pub fn execute(action: Option<ServicesCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(ServicesCommands::Status { json: false }) {
        ServicesCommands::Status { json } => status(json),
        ServicesCommands::Restart { name } => restart(&name),
        ServicesCommands::Stop { name } => stop(&name),
        ServicesCommands::Start { name } => start(&name),
        ServicesCommands::Logs { name, lines } => logs(&name, lines),
    }
}

fn status(json: bool) -> Result<()> {
    let services = service_table::status();
    if json {
        let body: Vec<serde_json::Value> = services
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "port": s.port,
                    "pid": s.pid,
                    "external": s.external,
                    "restarts": s.restarts,
                    "uptime": s.uptime(),
                    "rss": s.rss(),
                    "up": s.up,
                    "stopped": s.stopped,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} UP",
        "SERVICE", "PORT", "PID", "RESTARTS", "UPTIME", "RSS"
    );
    let any_external = services.iter().any(|s| s.external);
    for s in &services {
        let pid = match (s.external, s.pid) {
            (true, Some(p)) => format!("{p}*"),
            (true, None) => "ext*".to_string(),
            (false, Some(p)) => p.to_string(),
            (false, None) => "-".to_string(),
        };
        println!(
            "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} {}",
            s.name,
            s.port,
            pid,
            s.restarts,
            s.uptime(),
            s.rss(),
            if s.stopped {
                "stopped"
            } else if s.up {
                "yes"
            } else {
                "no"
            }
        );
    }
    if any_external {
        println!("* external process — bound outside daemon supervision");
    }
    Ok(())
}

/// Operator control of a supervised service: one implementation with the
/// MCP `services_*` tools (`susi_daemon::service_control`), which keeps the
/// supervised-only and daemon-running guards and terminates with the
/// platform's own process tool on every OS.
fn control(action: &str, name: &str) -> Result<()> {
    let report =
        susi_daemon::service_control::control(action, name).map_err(crate::anyhow::Error::msg)?;
    println!("{}", report["message"].as_str().unwrap_or_default());
    Ok(())
}

fn restart(name: &str) -> Result<()> {
    control("restart", name)
}

/// Terminate a supervised pid AND mark the row stopped so the monitor
/// leaves it down (`susi_daemon::service_control::stop`).
fn stop(name: &str) -> Result<()> {
    control("stop", name)
}

/// Clear the operator stop flag — the monitor respawns the service on
/// its next pass (fresh restart budget, cooldown cleared).
fn start(name: &str) -> Result<()> {
    control("start", name)
}

fn logs(name: &str, lines: usize) -> Result<()> {
    // "supervisor" is a valid pseudo-service — its log carries spawn,
    // respawn, hold-down, and adoption decisions.
    let file_name = if name == "supervisor" {
        "supervisor".to_string()
    } else {
        let Some(svc) = service_table::leaf_service(name) else {
            bail!("unknown service `{name}` (expected one of the leaf services, or `supervisor`)");
        };
        svc.name.to_string()
    };
    let path = susi_paths::SusiDirs::substrate_home()
        .join("logs")
        .join(format!("{file_name}.log"));
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => bail!(
            "no log at {} — the service may never have been spawned under supervision",
            path.display()
        ),
    };
    // Tail-bounded read: logs can grow unboundedly under supervision, so seek
    // to the last TAIL_BYTES instead of loading the whole file.
    const TAIL_BYTES: u64 = 512 * 1024;
    use std::io::{Read, Seek, SeekFrom};
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut file = file;
    let truncated = size > TAIL_BYTES;
    if truncated {
        file.seek(SeekFrom::End(-(TAIL_BYTES as i64)))
            .map_err(|e| crate::anyhow::anyhow!("seek log: {e}"))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| crate::anyhow::anyhow!("read log: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    // Drop the first partial line when we started mid-file.
    let start = all.len().saturating_sub(lines).max(usize::from(truncated));
    if truncated {
        println!("… (showing last {TAIL_BYTES} bytes of {})", path.display());
    }
    for line in &all[start..] {
        println!("{line}");
    }
    Ok(())
}
