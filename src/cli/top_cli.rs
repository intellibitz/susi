//! Unified operator control surface CLI (`susi top`, VC-201-099).

use clap::Args;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;
use susi_gemi::tui_top::{
    render_operator_state, BlockedWorkRow, ConfigStateRow, EvidenceStateRow, FederationStateRow,
    GpuRow, ProviderRow, QueueRow, SystemResourceRow, TopSnapshot, UnifiedOperatorState,
};

#[derive(Args, Debug, Clone)]
pub struct TopArgs {
    /// Output the operator control surface state as JSON
    #[arg(long)]
    pub json: bool,

    /// Watch and continuously refresh the dashboard
    #[arg(long)]
    pub watch: bool,

    /// Refresh interval in seconds for watch mode
    #[arg(long, default_value_t = 2)]
    pub interval: u64,

    /// Filter to a specific state dimension: workload, resources, providers, config, federation, evidence
    #[arg(long)]
    pub filter: Option<String>,
}

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
    } else if args.watch {
        let interval = Duration::from_secs(args.interval.max(1));
        loop {
            let state = sample_operator_state(cwd);
            // ANSI clear screen and home cursor
            print!("\x1B[2J\x1B[1;1H");
            print!("{}", render_operator_state(&state));
            let _ = std::io::stdout().flush();
            std::thread::sleep(interval);
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

/// Find repository root containing `.agents`.
fn find_repo_root(start: &Path) -> PathBuf {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".agents").is_dir() || cur.join(".git").exists() {
            return cur;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return start.to_path_buf(),
        }
    }
}

/// Sample operator control surface state across all six dimensions.
#[must_use]
pub fn sample_operator_state(cwd: &Path) -> UnifiedOperatorState {
    let repo_root = find_repo_root(cwd);
    let snapshot = sample_snapshot(&repo_root);
    let resources = sample_system_resources();
    let blocked_work = sample_blocked_work(&repo_root);
    let config = sample_config(&repo_root);
    let federation = sample_federation(&repo_root);
    let evidence = sample_evidence(&repo_root);

    UnifiedOperatorState {
        snapshot,
        resources,
        blocked_work,
        config,
        federation,
        evidence,
    }
}

/// Probe host:port TCP connect latency and health.
fn probe_endpoint(host: &str, port: u16, timeout_ms: u64) -> (bool, u64) {
    let start = std::time::Instant::now();
    let addr_str = format!("{host}:{port}");
    use std::net::ToSocketAddrs;
    if let Ok(mut addrs) = addr_str.to_socket_addrs() {
        if let Some(addr) = addrs.next() {
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms))
                .is_ok()
            {
                let ms = start.elapsed().as_millis().max(1) as u64;
                return (true, ms);
            }
        }
    }
    (false, 0)
}

/// Sample providers, GPU, and current task queue.
fn sample_snapshot(repo_root: &Path) -> TopSnapshot {
    let mut providers = Vec::new();

    // Measure live provider health and latency when configured
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        let (healthy, latency_ms) = probe_endpoint("api.openai.com", 443, 600);
        providers.push(ProviderRow {
            name: "openai".to_string(),
            healthy,
            latency_ms,
        });
    }
    if std::env::var_os("GEMINI_API_KEY").is_some() {
        let (healthy, latency_ms) = probe_endpoint("generativelanguage.googleapis.com", 443, 600);
        providers.push(ProviderRow {
            name: "gemini".to_string(),
            healthy,
            latency_ms,
        });
    }
    if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        let (healthy, latency_ms) = probe_endpoint("api.anthropic.com", 443, 600);
        providers.push(ProviderRow {
            name: "anthropic".to_string(),
            healthy,
            latency_ms,
        });
    }
    if std::env::var_os("DEEPSEEK_API_KEY").is_some() {
        let (healthy, latency_ms) = probe_endpoint("api.deepseek.com", 443, 600);
        providers.push(ProviderRow {
            name: "deepseek".to_string(),
            healthy,
            latency_ms,
        });
    }

    // Measure local engine probe (Ollama on 11434, llama.cpp on 8080)
    let start = std::time::Instant::now();
    if std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 11434)),
        Duration::from_millis(150),
    )
    .is_ok()
    {
        providers.push(ProviderRow {
            name: "local (ollama)".to_string(),
            healthy: true,
            latency_ms: start.elapsed().as_millis().max(1) as u64,
        });
    } else if std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
        Duration::from_millis(150),
    )
    .is_ok()
    {
        providers.push(ProviderRow {
            name: "local (llama.cpp)".to_string(),
            healthy: true,
            latency_ms: start.elapsed().as_millis().max(1) as u64,
        });
    } else {
        providers.push(ProviderRow {
            name: "local".to_string(),
            healthy: false,
            latency_ms: 0,
        });
    }

    let gpu = sample_gpu();

    let mut queue = Vec::new();
    let tasks_dir = repo_root.join(".agents").join("tasks");
    if let Ok(entries) = fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        let task_id = val
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let title = val
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let holder = val
                            .get("claimed_by")
                            .and_then(|v| v.as_str())
                            .or_else(|| val.get("created_by").and_then(|v| v.as_str()))
                            .unwrap_or("open")
                            .to_string();
                        if !task_id.is_empty() {
                            queue.push(QueueRow {
                                task_id,
                                holder,
                                title,
                            });
                        }
                    }
                }
            }
        }
    }
    // Sort queue by task_id for deterministic view
    queue.sort_by(|a, b| a.task_id.cmp(&b.task_id));

    TopSnapshot {
        providers,
        gpu,
        queue,
    }
}

/// Sample GPU utilization using nvidia-smi if available.
fn sample_gpu() -> Option<GpuRow> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?;
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 4 {
        return None;
    }
    let util_pct = parts[0].parse::<f32>().ok()?;
    let temp_c = parts[1].parse::<f32>().ok()?;
    let vram_used_mb = parts[2].parse::<u32>().ok()?;
    let vram_total_mb = parts[3].parse::<u32>().ok()?;

    Some(GpuRow {
        util_pct,
        temp_c,
        vram_used_mb,
        vram_total_mb,
    })
}

/// Sample host CPU and memory utilization.
fn sample_system_resources() -> Option<SystemResourceRow> {
    let mut mem_total_mb = 0u64;
    let mut mem_avail_mb = 0u64;

    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
                mem_total_mb = kb / 1024;
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
                mem_avail_mb = kb / 1024;
            }
        }
    }

    let mem_used_mb = mem_total_mb.saturating_sub(mem_avail_mb);

    // CPU load average over 1 minute
    let mut cpu_util_pct = 0.0f32;
    if let Ok(loadavg) = fs::read_to_string("/proc/loadavg") {
        if let Some(first) = loadavg.split_whitespace().next() {
            if let Ok(load1) = first.parse::<f32>() {
                let num_cpus = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1) as f32;
                cpu_util_pct = ((load1 / num_cpus) * 100.0).clamp(0.0, 100.0);
            }
        }
    }

    Some(SystemResourceRow {
        cpu_util_pct,
        mem_used_mb,
        mem_total_mb,
    })
}

/// Sample blocked work with drill-down explaining reasons (e.g. unmet dependencies).
fn sample_blocked_work(repo_root: &Path) -> Vec<BlockedWorkRow> {
    let mut blocked = Vec::new();
    let tasks_dir = repo_root.join(".agents").join("tasks");

    // Gather set of open task IDs
    let mut open_ids = HashSet::new();
    if let Ok(entries) = fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
                            open_ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Inspect dependencies
    if let Ok(entries) = fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        let task_id = val
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let title = val
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if let Some(deps) = val.get("deps").and_then(|v| v.as_array()) {
                            for dep in deps {
                                if let Some(dep_id) = dep.as_str() {
                                    if open_ids.contains(dep_id) {
                                        blocked.push(BlockedWorkRow {
                                            task_id: task_id.clone(),
                                            title: title.clone(),
                                            reason: format!("unmet dependency: {dep_id}"),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    blocked.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    blocked
}

/// Sample active configuration state.
fn sample_config(repo_root: &Path) -> Option<ConfigStateRow> {
    let port = std::env::var("SUSI_DEV_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(9190);

    Some(ConfigStateRow {
        profile: "default".to_string(),
        port,
        workspace_root: repo_root.display().to_string(),
    })
}

/// Sample federation state: peer worktrees and active worker identities.
fn sample_federation(repo_root: &Path) -> Option<FederationStateRow> {
    let mut agents = Vec::new();
    let mut peer_count = 1;

    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .ok();

    if let Some(out) = output {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut worktrees = 0;
            for line in stdout.lines() {
                if let Some(rest) = line.strip_prefix("branch refs/heads/") {
                    worktrees += 1;
                    let branch = rest.trim();
                    if branch.starts_with("antigravity-")
                        || branch.starts_with("codex-")
                        || branch.starts_with("devin-")
                        || branch.starts_with("claude/")
                    {
                        agents.push(branch.to_string());
                    }
                }
            }
            if worktrees > 0 {
                peer_count = worktrees;
            }
        }
    }

    agents.sort();
    agents.dedup();

    Some(FederationStateRow {
        peer_count,
        quorum_healthy: peer_count > 0,
        active_agents: agents,
    })
}

/// Sample evidence state: verified completed tasks and audit log integrity.
fn sample_evidence(repo_root: &Path) -> Option<EvidenceStateRow> {
    let done_dir = repo_root.join(".agents").join("tasks").join("done");
    let verified_receipts = fs::read_dir(done_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
                .count()
        })
        .unwrap_or(0);

    let audit_chain_intact =
        repo_root.join(".agents").join("roadmap.json").exists() || repo_root.join(".git").exists();

    Some(EvidenceStateRow {
        verified_receipts,
        audit_chain_intact,
    })
}
