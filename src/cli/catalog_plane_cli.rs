//! Shared launch / worker helpers for catalog control planes
//! (`susi agents`, `susi frameworks`, and single-agent wrappers).

use crate::anyhow::{bail, Result};
use std::process::{Command, Stdio};
use susi_gawd::external::{AgentManager, RunStatus};

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

/// Foreground worker with `tokio::signal` graceful shutdown.
pub fn run_worker(manager: AgentManager, id: &str, fail_label: &str) -> Result<()> {
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let signal_shutdown = std::sync::Arc::clone(&shutdown);
    let signal_thread = std::thread::spawn(move || {
        rt.block_on(watch_worker_signals(signal_shutdown, stop_rx));
    });
    let exec_result = manager.with_shutdown(shutdown).execute(id);
    drop(stop_tx);
    let _ = signal_thread.join();
    let run = exec_result?;
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

async fn watch_worker_signals(
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use std::sync::atomic::Ordering;
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = signal(SignalKind::interrupt()).ok();
        let mut sigterm = signal(SignalKind::terminate()).ok();
        let mut sighup = signal(SignalKind::hangup()).ok();
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                Some(()) = async {
                    match sigint.as_mut() {
                        Some(s) => s.recv().await,
                        None => std::future::pending().await,
                    }
                } => shutdown.store(true, Ordering::Release),
                Some(()) = async {
                    match sigterm.as_mut() {
                        Some(s) => s.recv().await,
                        None => std::future::pending().await,
                    }
                } => shutdown.store(true, Ordering::Release),
                Some(()) = async {
                    match sighup.as_mut() {
                        Some(s) => s.recv().await,
                        None => std::future::pending().await,
                    }
                } => shutdown.store(true, Ordering::Release),
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = &mut stop_rx => {}
            res = tokio::signal::ctrl_c() => {
                if res.is_ok() {
                    shutdown.store(true, Ordering::Release);
                }
            }
        }
    }
}
