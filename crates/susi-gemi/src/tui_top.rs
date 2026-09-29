//! Headless render model for `susi top` (providers, GPU, queue).

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopSnapshot {
    pub providers: Vec<ProviderRow>,
    pub gpu: Option<GpuRow>,
    pub queue: Vec<QueueRow>,
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
