//! `susi services` — the process-table view of the microkernel's leaf
//! services: which are supervised by the daemon, which ports are live.
//! `restart` SIGTERMs a supervised pid; the daemon's supervisor notices
//! the death on its next probe and respawns it.

use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use susi_core::service_table;

#[derive(Debug, Subcommand)]
pub enum ServicesCommands {
    /// Show every leaf service, its supervised pid, and live health (default)
    Status,
    /// Terminate a supervised service; the daemon respawns it on its next probe
    Restart {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
    },
    /// Tail a supervised service's log file (the daemon redirects each
    /// spawned service's stderr to `substrate_home/logs/<name>.log`)
    Logs {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
        /// Number of trailing lines to print
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
}

pub fn execute(action: Option<ServicesCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(ServicesCommands::Status) {
        ServicesCommands::Status => status(),
        ServicesCommands::Restart { name } => restart(&name),
        ServicesCommands::Logs { name, lines } => logs(&name, lines),
    }
}

fn status() -> Result<()> {
    println!(
        "{:<14} {:<6} {:<8} {:<9} {:<8} UP",
        "SERVICE", "PORT", "PID", "RESTARTS", "UPTIME"
    );
    for s in service_table::status() {
        let pid = s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "{:<14} {:<6} {:<8} {:<9} {:<8} {}",
            s.name,
            s.port,
            pid,
            s.restarts,
            s.uptime(),
            if s.up { "yes" } else { "no" }
        );
    }
    Ok(())
}

fn restart(name: &str) -> Result<()> {
    let Some(svc) = service_table::leaf_service(name) else {
        bail!("unknown service `{name}` (expected one of the leaf services)");
    };
    let table = service_table::load();
    let Some(rec) = table.iter().find(|r| r.name == svc.name) else {
        bail!(
            "{name} is not supervised by the daemon — nothing to restart \
             (start the daemon with `susi start` to put it under supervision)"
        );
    };
    #[cfg(unix)]
    {
        let ok = std::process::Command::new("kill")
            .args(["-TERM", &rec.pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            bail!("failed to SIGTERM {name} (pid {})", rec.pid);
        }
        println!(
            "sent SIGTERM to {name} (pid {}); the daemon will respawn it",
            rec.pid
        );
    }
    #[cfg(not(unix))]
    {
        bail!("restart is unix-only for now (pid {})", rec.pid);
    }
    Ok(())
}

fn logs(name: &str, lines: usize) -> Result<()> {
    let Some(svc) = service_table::leaf_service(name) else {
        bail!("unknown service `{name}` (expected one of the leaf services)");
    };
    let path = susi_paths::SusiDirs::substrate_home()
        .join("logs")
        .join(format!("{}.log", svc.name));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => bail!(
            "no log at {} — the service may never have been spawned under supervision",
            path.display()
        ),
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    for line in &all[start..] {
        println!("{line}");
    }
    Ok(())
}
