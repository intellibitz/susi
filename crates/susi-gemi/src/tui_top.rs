//! Headless render model for `susi top` (providers, GPU, queue, and unified operator control surface).

use serde::{Deserialize, Serialize};

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
