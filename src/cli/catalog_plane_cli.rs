//! Shared launch / worker helpers for catalog control planes
//! (`susi agents`, `susi frameworks`, and single-agent wrappers).

use anyhow::{bail, Result};
use std::process::{Command, Stdio};
use susi_agents::external::{AgentManager, RunStatus};

use crate::cli_json::print_json;

/// Options for [`launch`].
pub struct LaunchOpts<'a> {
    pub wait: bool,
    pub worker_argv: &'a [&'a str],
    pub banner: Option<&'a str>,
    pub fail_label: &'a str,
}

/// Detach a worker subprocess or run in-process when `opts.wait` is set.
pub fn launch(manager: &AgentManager, id: &str, opts: LaunchOpts<'_>) -> Result<()> {
    print_json(&manager.read(id)?)?;
    if opts.wait {
        return run_worker(manager.clone(), id, opts.fail_label);
    }
    // `/proc/self/exe` re-executes the live inode; `current_exe` reports a
    // `… (deleted)` path after an in-place binary replacement (ENOENT).
    let exe = susi_daemon::supervisor::reexec_path()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    let mut command = Command::new(exe);
    command
        .args(opts.worker_argv)
        .current_dir(&manager.read(id)?.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(banner) = opts.banner {
        if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
            command.env("SUSI_PROCESS_BANNER", banner);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            manager.mark_launch_failed(id, &e.to_string())?;
            return Err(e.into());
        }
    }
    Ok(())
}

/// Foreground worker with signal-hook shutdown (Unix).
pub fn run_worker(manager: AgentManager, id: &str, fail_label: &str) -> Result<()> {
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, shutdown.clone())?;
    }
    let run = manager.with_shutdown(shutdown).execute(id)?;
    print_json(&run)?;
    if matches!(
        run.status,
        RunStatus::Failed | RunStatus::Unknown | RunStatus::Cancelled
    ) {
        bail!(
            "{fail_label} task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
