//! Shared launch / worker helpers for catalog control planes
//! (`susi agents`, `susi frameworks`, and single-agent wrappers).

use crate::anyhow::{bail, Result};
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use susi_core::plane_bus_ipc::IpcPlaneBus;
use susi_gawd::external::{AgentManager, CatalogKind, RunRecord, RunStatus};

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
    let signal_manager = manager.clone();
    let signal_id = id.to_owned();
    let signal_thread = std::thread::spawn(move || {
        rt.block_on(watch_worker_signals(
            || {
                signal_shutdown.store(true, std::sync::atomic::Ordering::Release);
                if let Err(error) = signal_manager.signal_cancel(&signal_id) {
                    eprintln!("signal worker {signal_id}: {error}");
                }
            },
            stop_rx,
        ));
    });
    let exec_result = execute_worker(&manager.with_shutdown(shutdown), id);
    drop(stop_tx);
    let _ = signal_thread.join();
    let (run, receipt) = exec_result?;
    if let Some(receipt) = receipt {
        print_json(&receipt)?;
    } else {
        print_json(&run)?;
    }
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

const REGISTERED_WORKER: &str = "fleet_external_run_v1";

/// Only healthy absence can select another transport. After dispatch, the
/// native record must confirm the returned run before the operator sees success.
fn execute_worker(manager: &AgentManager, id: &str) -> Result<(RunRecord, Option<Value>)> {
    if let Some(scope) = susi_core::scoped_tools::current() {
        scope.check().map_err(crate::anyhow::Error::msg)?;
        return Ok((manager.execute(id)?, None));
    }
    let kind = match manager.kind() {
        CatalogKind::Execution => "execution",
        CatalogKind::Framework => "framework",
    };
    let workspace =
        susi_paths::native_path::encode(manager.workspace()).map_err(crate::anyhow::Error::msg)?;
    let args = json!({"workspace": workspace, "kind": kind, "run_id": id});
    susi_harness::register_tool_backend();
    let reply = susi_config::mcp_session::try_call_reply_with_info(
        REGISTERED_WORKER,
        &args,
        Some(manager.workspace()),
        Default::default(),
    )?;
    let receipt = if let Some(reply) = reply {
        if reply.result["isError"].as_bool() == Some(true) {
            bail!(
                "{}",
                reply.result["content"][0]["text"]
                    .as_str()
                    .unwrap_or("registered worker refused the task")
            );
        }
        susi_config::mcp_session::json_result(reply.result, reply.protocol_version.as_deref())?
    } else if let Some(receipt) = IpcPlaneBus::global()
        .try_request_exact_once(REGISTERED_WORKER, args)
        .map_err(crate::anyhow::Error::msg)?
    {
        receipt
    } else {
        return Ok((manager.execute(id)?, None));
    };
    let (committed, native) = manager.read_receipt(id)?;
    confirm_worker_receipt(&native, &receipt)?;
    Ok((committed, Some(native)))
}

fn confirm_worker_receipt(native: &Value, receipt: &Value) -> Result<()> {
    let same = native
        .as_object()
        .zip(receipt.as_object())
        .is_some_and(|(stored, reply)| {
            // The owner timestamps the atomic save separately from constructing its reply.
            stored.len() == reply.len()
                && reply.get("updated_at").is_some_and(Value::is_u64)
                && reply
                    .iter()
                    .all(|(key, value)| key == "updated_at" || stored.get(key) == Some(value))
        });
    if !same || native["status"] == "queued" {
        bail!("registered worker response does not match the durable task");
    }
    Ok(())
}

async fn watch_worker_signals(
    on_signal: impl Fn(),
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
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
                } => on_signal(),
                Some(()) = async {
                    match sigterm.as_mut() {
                        Some(s) => s.recv().await,
                        None => std::future::pending().await,
                    }
                } => on_signal(),
                Some(()) = async {
                    match sighup.as_mut() {
                        Some(s) => s.recv().await,
                        None => std::future::pending().await,
                    }
                } => on_signal(),
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = &mut stop_rx => {}
            res = tokio::signal::ctrl_c() => {
                if res.is_ok() {
                    on_signal();
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/catalog_plane_worker.rs"]
mod tests;
