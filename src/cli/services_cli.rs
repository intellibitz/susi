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
    /// Stop a supervised service and hold it down — the supervisor will
    /// not respawn it until `susi services start` clears the stop flag.
    /// Distinct from restart: the service stays dead by operator intent.
    Stop {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
    },
    /// Clear an operator stop and let the supervisor bring the service up
    Start {
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
        ServicesCommands::Stop { name } => stop(&name),
        ServicesCommands::Start { name } => start(&name),
        ServicesCommands::Logs { name, lines } => logs(&name, lines),
    }
}

fn status() -> Result<()> {
    println!(
        "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} UP",
        "SERVICE", "PORT", "PID", "RESTARTS", "UPTIME", "RSS"
    );
    let services = service_table::status();
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
    if rec.external {
        // The "never kill what we didn't spawn" rule applies to operator
        // restarts too — an external process is not ours to signal.
        bail!(
            "{name} is bound by an external process (pid {}) that the daemon \
             does not supervise — stop it yourself and the supervisor will \
             spawn a managed service on the freed port",
            rec.pid
        );
    }
    if susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir()).is_none() {
        bail!(
            "the daemon is not running — killing {name} (pid {}) would leave it \
             dead with nothing to respawn it; start the daemon with `susi start` first",
            rec.pid
        );
    }
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

/// SIGTERM a supervised pid AND mark the row stopped so the monitor
/// leaves it down. Same guards as restart: supervised-only (external
/// rows are not ours to signal) and the daemon must be running — the
/// stop flag is meaningless if nothing honors it.
fn stop(name: &str) -> Result<()> {
    let Some(svc) = service_table::leaf_service(name) else {
        bail!("unknown service `{name}` (expected one of the leaf services)");
    };
    if susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir()).is_none() {
        bail!(
            "the daemon is not running — {name} is already unsupervised; \
             kill its pid directly if it is still alive"
        );
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
    .map_err(|e| anyhow::anyhow!("{e}"))?
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    #[cfg(unix)]
    {
        if service_table::pid_alive(pid) {
            let ok = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                bail!("{name} marked stopped but SIGTERM pid {pid} failed");
            }
        }
        println!(
            "stopped {name} (pid {pid}); supervisor holds it down until `susi services start {name}`"
        );
    }
    #[cfg(not(unix))]
    {
        bail!("stop is unix-only for now (pid {pid})");
    }
    Ok(())
}

/// Clear the operator stop flag — the monitor respawns the service on
/// its next pass (fresh restart budget, cooldown cleared).
fn start(name: &str) -> Result<()> {
    let Some(svc) = service_table::leaf_service(name) else {
        bail!("unknown service `{name}` (expected one of the leaf services)");
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
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    match cleared {
        Some(true) => {
            println!("cleared stop on {name}; the supervisor will respawn it within ~5s");
            return Ok(());
        }
        Some(false) => {
            println!("{name} is not stopped");
            return Ok(());
        }
        None => {}
    }
    // No record at all — dropping the name into the table as stopped=false
    // adds nothing; the monitor's missing-service path already retries
    // unsupervised services on a 30s cadence. A record-less `start` is a
    // no-op the operator should hear about honestly.
    println!(
        "{name} has no process-table row — the supervisor retries missing \
         services on its own; nothing was changed"
    );
    Ok(())
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
            .map_err(|e| anyhow::anyhow!("seek log: {e}"))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| anyhow::anyhow!("read log: {e}"))?;
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
