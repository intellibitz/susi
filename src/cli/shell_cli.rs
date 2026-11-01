//! Persistent Pulse Shell (`susi` with no args / `susi shell`) plus the
//! `MissionHost` context passed to post-boot dispatchers.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use susi::SUSI_VERSION;
use susi_sandbox::manager::SusiConfig;

/// Post-boot context handed to `mission_cli`/`admin_cli` dispatchers: the
/// live swarm master agent, loaded config, and the two canonical dirs.
/// `susi_gawd::ama::SusiMasterAgent::new()` is the hook-wiring facade — it
/// returns the swarm agent, which is the type held here.
pub(crate) struct MissionHost<'a> {
    pub(crate) cwd: &'a Path,
    pub(crate) global_dir: &'a Path,
    pub(crate) ama: &'a susi_gawd::swarm::ama::SusiMasterAgent,
    pub(crate) cfg: &'a SusiConfig,
}

pub(crate) fn glass_box_callback(piece: String) {
    print!("{}", piece);
    let _ = io::stdout().flush();
}

pub(crate) fn run_shell(workspace: &Path) {
    println!("Susi {SUSI_VERSION}");
    println!("What would you like to achieve?");
    crate::cli::setup_cli::print_readiness();
    println!("Type 'exit' to quit.");
    let _discovery = DiscoveryRefresh::start(workspace);

    loop {
        print!("susi> ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => (),
            Err(error) => {
                eprintln!("Could not read your goal: {error}");
                break;
            }
        }
        let goal = input.trim();
        if goal.is_empty() {
            continue;
        }
        if goal == "exit" || goal == "quit" {
            break;
        }
        println!("Goal received. Starting work...");
        let _ = io::stdout().flush();
        // Reuse the actual mission entry point, including governance, evidence
        // and exit status. Opening the shell creates no detached consumer and
        // exiting cannot silently discard a queued goal. `--` keeps a goal
        // such as "status" as intent rather than a CLI control command.
        let outcome = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .arg("--")
                .arg(goal)
                .current_dir(workspace)
                .stdin(std::process::Stdio::null())
                .status()
        });
        match outcome {
            Ok(status) if status.success() => (),
            Ok(_) => eprintln!(
                "This goal did not complete. See the reason above; you can try another goal."
            ),
            Err(error) => eprintln!("Could not start this goal: {error}"),
        }
    }
}

/// Refresh only local discovery while the user thinks. Optional agent and
/// framework catalogs do not belong on this path. The process boundary gives
/// even a broken host probe a firm deadline and lets closing the shell reap it.
struct DiscoveryRefresh {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl DiscoveryRefresh {
    fn start(workspace: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stop);
        let workspace = workspace.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("opening-discovery".into())
            .spawn(move || refresh_local_discovery(&workspace, &cancelled))
            .ok();
        Self { stop, worker }
    }
}

impl Drop for DiscoveryRefresh {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn refresh_local_discovery(workspace: &Path, stop: &AtomicBool) {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    if stop.load(Ordering::Acquire) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Ok(mut child) = Command::new(exe)
        .args(["ecosystem", "scan"])
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // Drain output while the child runs so the pipe cannot hold up discovery.
    let output = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.take(128 * 1024).read_to_end(&mut bytes);
            bytes
        })
    });
    let start = Instant::now();
    let succeeded = loop {
        if stop.load(Ordering::Acquire) || start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = output
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if stop.load(Ordering::Acquire) {
        return;
    }
    let report = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    if !succeeded || report.is_none() {
        eprintln!("\nLocal discovery is unavailable. You can still give Susi a goal; availability will be checked before execution.");
        return;
    }
    let engines = report
        .as_ref()
        .and_then(|report| report["engines"].as_array());
    let running: Vec<&str> = engines
        .into_iter()
        .flatten()
        .filter(|engine| engine["running"].as_bool() == Some(true))
        .filter_map(|engine| engine["name"].as_str())
        .collect();
    if running.is_empty() {
        eprintln!("\nLocal discovery complete: no running external AI engine found. Cloud connections and Susi's native models are checked when you submit a goal.");
    } else {
        eprintln!("\nLocal discovery complete: {} running. Model readiness is checked when you submit a goal.", running.join(", "));
    }
}
