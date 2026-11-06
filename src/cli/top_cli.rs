//! Unified operator control surface CLI (`susi top`, VC-201-099).

use std::path::Path;
use std::process::ExitCode;
use susi_gemi::operator_report::{render_operator_state, sample_operator_state};

pub use susi_cli_defs::top::*;

/// Execute `susi top`.
pub fn execute(args: TopArgs, cwd: &Path) -> ExitCode {
    if args.json {
        let state = sample_operator_state(cwd);
        match serde_json::to_string_pretty(&state) {
            Ok(json_str) => {
                println!("{json_str}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("Failed to serialize operator state to JSON: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        let state = sample_operator_state(cwd);
        if let Some(ref dim) = args.filter {
            match dim.to_lowercase().as_str() {
                "workload" => {
                    println!("── workload ──");
                    if state.snapshot.queue.is_empty() {
                        println!("  queue: (idle)");
                    } else {
                        println!("  active tasks:");
                        for q in &state.snapshot.queue {
                            println!("    {} [{}] {}", q.task_id, q.holder, q.title);
                        }
                    }
                    if state.blocked_work.is_empty() {
                        println!("  blocked: (none)");
                    } else {
                        println!("  blocked work:");
                        for b in &state.blocked_work {
                            println!("    {} — {} ({})", b.task_id, b.title, b.reason);
                        }
                    }
                }
                "resources" => {
                    println!("── resources ──");
                    if let Some(r) = &state.resources {
                        println!(
                            "  cpu: {:.1}% | ram: {}/{} MB",
                            r.cpu_util_pct, r.mem_used_mb, r.mem_total_mb
                        );
                    }
                    if let Some(g) = &state.snapshot.gpu {
                        println!(
                            "  gpu: util={:.0}% temp={:.0}C vram={}/{}MB",
                            g.util_pct, g.temp_c, g.vram_used_mb, g.vram_total_mb
                        );
                    }
                }
                "providers" => {
                    println!("── providers ──");
                    for p in &state.snapshot.providers {
                        println!(
                            "  {} {} {}ms",
                            p.name,
                            if p.healthy { "ok" } else { "down" },
                            p.latency_ms
                        );
                    }
                }
                "config" | "configuration" => {
                    println!("── configuration ──");
                    if let Some(c) = &state.config {
                        println!(
                            "  profile: {} | port: {} | root: {}",
                            c.profile, c.port, c.workspace_root
                        );
                    }
                }
                "federation" => {
                    println!("── federation ──");
                    if let Some(f) = &state.federation {
                        println!(
                            "  peers: {} | quorum: {} | agents: {}",
                            f.peer_count,
                            if f.quorum_healthy { "ok" } else { "degraded" },
                            f.active_agents.join(", ")
                        );
                    }
                }
                "evidence" => {
                    println!("── evidence ──");
                    if let Some(e) = &state.evidence {
                        println!(
                            "  verified receipts: {} | audit chain: {}",
                            e.verified_receipts,
                            if e.audit_chain_intact {
                                "intact"
                            } else {
                                "compromised"
                            }
                        );
                    }
                }
                other => {
                    eprintln!("Unknown filter dimension: '{other}'. Choose from: workload, resources, providers, config, federation, evidence.");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            print!("{}", render_operator_state(&state));
        }
        ExitCode::SUCCESS
    }
}
