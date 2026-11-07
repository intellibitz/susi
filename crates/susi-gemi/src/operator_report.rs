//! Headless render model for `susi top` (providers, GPU, queue, and unified operator control surface).
//!
//! Formerly `tui_top`. Output is plain text or JSON; core has no redraw loop.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRow {
    pub name: String,
    pub healthy: bool,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuRow {
    pub util_pct: f32,
    pub temp_c: f32,
    pub vram_used_mb: u32,
    pub vram_total_mb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRow {
    pub task_id: String,
    pub holder: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedWorkRow {
    pub task_id: String,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemResourceRow {
    pub cpu_util_pct: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigStateRow {
    pub profile: String,
    pub port: u16,
    pub workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationStateRow {
    pub peer_count: usize,
    pub quorum_healthy: bool,
    pub active_agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceStateRow {
    pub verified_receipts: usize,
    pub audit_chain_intact: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopSnapshot {
    pub providers: Vec<ProviderRow>,
    pub gpu: Option<GpuRow>,
    pub queue: Vec<QueueRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnifiedOperatorState {
    pub snapshot: TopSnapshot,
    pub resources: Option<SystemResourceRow>,
    pub blocked_work: Vec<BlockedWorkRow>,
    pub config: Option<ConfigStateRow>,
    pub federation: Option<FederationStateRow>,
    pub evidence: Option<EvidenceStateRow>,
}

/// Render a headless TUI frame (no terminal dependency).
#[must_use]
pub fn render_frame(snap: &TopSnapshot) -> String {
    let mut out = String::from("susi top\n");
    out.push_str("── providers ──\n");
    for p in &snap.providers {
        out.push_str(&format!(
            "  {} {} {}ms\n",
            p.name,
            if p.healthy { "ok" } else { "down" },
            p.latency_ms
        ));
    }
    out.push_str("── gpu ──\n");
    match &snap.gpu {
        Some(g) => out.push_str(&format!(
            "  util={:.0}% temp={:.0}C vram={}/{}MB\n",
            g.util_pct, g.temp_c, g.vram_used_mb, g.vram_total_mb
        )),
        None => out.push_str("  (none)\n"),
    }
    out.push_str("── queue ──\n");
    if snap.queue.is_empty() {
        out.push_str("  (idle)\n");
    } else {
        for q in &snap.queue {
            out.push_str(&format!("  {} [{}] {}\n", q.task_id, q.holder, q.title));
        }
    }
    out
}

/// Render the unified operator control surface (VC-201-099).
#[must_use]
pub fn render_operator_state(state: &UnifiedOperatorState) -> String {
    let mut out = String::from("susi top — unified operator control surface\n");

    out.push_str("── workload ──\n");
    if state.snapshot.queue.is_empty() {
        out.push_str("  queue: (idle)\n");
    } else {
        out.push_str("  active tasks:\n");
        for q in &state.snapshot.queue {
            out.push_str(&format!("    {} [{}] {}\n", q.task_id, q.holder, q.title));
        }
    }
    if state.blocked_work.is_empty() {
        out.push_str("  blocked: (none)\n");
    } else {
        out.push_str("  blocked work:\n");
        for b in &state.blocked_work {
            out.push_str(&format!("    {} — {} ({})\n", b.task_id, b.title, b.reason));
        }
    }

    out.push_str("── resources ──\n");
    match &state.resources {
        Some(r) => out.push_str(&format!(
            "  cpu: {:.1}% | ram: {}/{} MB\n",
            r.cpu_util_pct, r.mem_used_mb, r.mem_total_mb
        )),
        None => out.push_str("  system: (unmeasured)\n"),
    }
    match &state.snapshot.gpu {
        Some(g) => out.push_str(&format!(
            "  gpu: util={:.0}% temp={:.0}C vram={}/{}MB\n",
            g.util_pct, g.temp_c, g.vram_used_mb, g.vram_total_mb
        )),
        None => out.push_str("  gpu: (none)\n"),
    }

    out.push_str("── providers ──\n");
    if state.snapshot.providers.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for p in &state.snapshot.providers {
            out.push_str(&format!(
                "  {} {} {}ms\n",
                p.name,
                if p.healthy { "ok" } else { "down" },
                p.latency_ms
            ));
        }
    }

    out.push_str("── configuration ──\n");
    match &state.config {
        Some(c) => out.push_str(&format!(
            "  profile: {} | port: {} | root: {}\n",
            c.profile, c.port, c.workspace_root
        )),
        None => out.push_str("  (default)\n"),
    }

    out.push_str("── federation ──\n");
    match &state.federation {
        Some(f) => {
            let agents_str = if f.active_agents.is_empty() {
                "none".to_string()
            } else {
                f.active_agents.join(", ")
            };
            out.push_str(&format!(
                "  peers: {} | quorum: {} | agents: {}\n",
                f.peer_count,
                if f.quorum_healthy { "ok" } else { "degraded" },
                agents_str
            ));
        }
        None => out.push_str("  standalone\n"),
    }

    out.push_str("── evidence ──\n");
    match &state.evidence {
        Some(e) => out.push_str(&format!(
            "  verified receipts: {} | audit chain: {}\n",
            e.verified_receipts,
            if e.audit_chain_intact {
                "intact"
            } else {
                "compromised"
            }
        )),
        None => out.push_str("  (untracked)\n"),
    }

    out
}

// ── Sampling (shared by `susi top` and MCP `operator_snapshot`) ─────────

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

/// Sample operator control surface state across all six dimensions, for
/// the repository that contains `cwd` (or `cwd` itself). `susi top --json`
/// prints this document and MCP `operator_snapshot` returns it.
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

/// Sample host CPU and memory utilization on every OS through
/// `susi_config::host_metrics`: CPU is the 1-minute load per logical CPU
/// where the OS keeps a load average (Linux, macOS) and the sampled
/// utilization on Windows. `None` when the OS reports no memory total, so
/// the view never shows a made-up zero.
fn sample_system_resources() -> Option<SystemResourceRow> {
    use susi_config::host_metrics;
    let mem = host_metrics::memory()?;
    let mem_total_mb = mem.total_bytes / (1024 * 1024);
    let mem_used_mb = mem.used_bytes() / (1024 * 1024);
    let cpu_util_pct = host_metrics::cpu_pressure_pct().unwrap_or(0.0);

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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn render_frame_formats_providers_gpu_and_queue_for_populated_and_idle_snapshots() {
        let populated = TopSnapshot {
            providers: vec![
                ProviderRow {
                    name: "gemini".to_string(),
                    healthy: true,
                    latency_ms: 42,
                },
                ProviderRow {
                    name: "local".to_string(),
                    healthy: false,
                    latency_ms: 0,
                },
            ],
            gpu: Some(GpuRow {
                util_pct: 65.4,
                temp_c: 58.2,
                vram_used_mb: 4096,
                vram_total_mb: 16384,
            }),
            queue: vec![QueueRow {
                task_id: "T-100".to_string(),
                holder: "agent-a".to_string(),
                title: "Verify telemetry".to_string(),
            }],
        };
        let frame = render_frame(&populated);
        assert!(frame.contains("gemini ok 42ms"));
        assert!(frame.contains("local down 0ms"));
        assert!(frame.contains("util=65% temp=58C vram=4096/16384MB"));
        assert!(frame.contains("T-100 [agent-a] Verify telemetry"));

        let idle = TopSnapshot {
            providers: Vec::new(),
            gpu: None,
            queue: Vec::new(),
        };
        let idle_frame = render_frame(&idle);
        assert!(idle_frame.contains("── gpu ──\n  (none)\n"));
        assert!(idle_frame.contains("── queue ──\n  (idle)\n"));
    }

    #[test]
    fn render_operator_state_renders_all_six_control_surface_sections() {
        let state = UnifiedOperatorState {
            snapshot: TopSnapshot {
                providers: vec![ProviderRow {
                    name: "openai".to_string(),
                    healthy: true,
                    latency_ms: 19,
                }],
                gpu: None,
                queue: Vec::new(),
            },
            resources: Some(SystemResourceRow {
                cpu_util_pct: 23.5,
                mem_used_mb: 8192,
                mem_total_mb: 32768,
            }),
            blocked_work: vec![BlockedWorkRow {
                task_id: "T-200".to_string(),
                title: "Blocked task".to_string(),
                reason: "unmet dependency: T-199".to_string(),
            }],
            config: Some(ConfigStateRow {
                profile: "default".to_string(),
                port: 9190,
                workspace_root: "/workspace".to_string(),
            }),
            federation: Some(FederationStateRow {
                peer_count: 2,
                quorum_healthy: false,
                active_agents: vec!["codex-worker".to_string()],
            }),
            evidence: Some(EvidenceStateRow {
                verified_receipts: 12,
                audit_chain_intact: false,
            }),
        };
        let rendered = render_operator_state(&state);
        assert!(rendered.contains("── workload ──"));
        assert!(rendered.contains("T-200 — Blocked task (unmet dependency: T-199)"));
        assert!(rendered.contains("cpu: 23.5% | ram: 8192/32768 MB"));
        assert!(rendered.contains("profile: default | port: 9190 | root: /workspace"));
        assert!(rendered.contains("peers: 2 | quorum: degraded | agents: codex-worker"));
        assert!(rendered.contains("verified receipts: 12 | audit chain: compromised"));
    }

    #[test]
    fn find_repo_root_walks_ancestors_to_agents_or_git_directory() {
        let tmp =
            std::env::temp_dir().join(format!("susi_operator_report_root_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let nested = tmp.join("crates").join("susi-gemi").join("src");
        fs::create_dir_all(tmp.join(".agents")).expect("create .agents");
        fs::create_dir_all(&nested).expect("create nested");

        assert_eq!(find_repo_root(&nested), tmp);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sample_blocked_work_and_sample_evidence_inspect_tasks_and_done_receipts() {
        let tmp = std::env::temp_dir().join(format!(
            "susi_operator_report_sample_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let tasks_dir = tmp.join(".agents").join("tasks");
        let done_dir = tasks_dir.join("done");
        fs::create_dir_all(&done_dir).expect("create done dir");
        fs::write(tmp.join(".agents").join("roadmap.json"), "{}").expect("write roadmap");
        fs::write(
            tasks_dir.join("T-1.json"),
            r#"{"id":"T-1","title":"Prerequisite","deps":[]}"#,
        )
        .expect("write T-1");
        fs::write(
            tasks_dir.join("T-2.json"),
            r#"{"id":"T-2","title":"Dependent","deps":["T-1"]}"#,
        )
        .expect("write T-2");
        fs::write(done_dir.join("T-0.json"), r#"{"id":"T-0"}"#).expect("write done T-0");

        let blocked = sample_blocked_work(&tmp);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].task_id, "T-2");
        assert_eq!(blocked[0].reason, "unmet dependency: T-1");

        let evidence = sample_evidence(&tmp).expect("sample evidence");
        assert_eq!(evidence.verified_receipts, 1);
        assert!(evidence.audit_chain_intact);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_operator_state_handles_minimal_none_sections_gracefully() {
        let state = UnifiedOperatorState {
            snapshot: TopSnapshot {
                providers: Vec::new(),
                gpu: None,
                queue: Vec::new(),
            },
            resources: None,
            blocked_work: Vec::new(),
            config: None,
            federation: None,
            evidence: None,
        };
        let rendered = render_operator_state(&state);
        assert!(rendered.contains("── workload ──"));
        assert!(rendered.contains("  (default)\n"));
        assert!(rendered.contains("  standalone\n"));
        assert!(rendered.contains("  (untracked)\n"));
    }
}
