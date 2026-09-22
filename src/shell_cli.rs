//! Persistent Pulse Shell (`susi` with no args / `susi shell`) plus the
//! `MissionHost` context passed to post-boot dispatchers.

use std::io::{self, Write};
use std::path::Path;

use susi::SUSI_VERSION;
use susi_gawd::ama::SusiMasterAgent;
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
    use susi_gawd::queue::SubstratePulseQueue;
    let queue = SubstratePulseQueue::global();
    let ama = SusiMasterAgent::new();

    println!(
        "SUSI Pulse Shell v{} (Glass Box Telemetry Mode Active)",
        SUSI_VERSION
    );
    println!(
        "Enter pulses to interact with the substrate. Pulses are queued and processed in order."
    );
    println!("Type 'exit' to quit.");

    std::thread::spawn(move || {
        queue.register_consumer();
        loop {
            let pulse = queue.pop_blocking();
            let _ = ama.solve_stream(
                &pulse.intent,
                &pulse.workspace,
                &pulse.version,
                &glass_box_callback,
            );
        }
    });

    loop {
        print!("susi> ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "exit" || trimmed == "quit" {
                break;
            }
            let _ = queue.ingest(trimmed, workspace, SUSI_VERSION);
        } else {
            break;
        }
    }
}
