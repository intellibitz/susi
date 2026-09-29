//! Headless `susi top` render coverage.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use susi_gemi::tui_top::{render_frame, GpuRow, ProviderRow, QueueRow, TopSnapshot};

#[test]
fn tui_top_renders_providers_gpu_and_queue() {
    let snap = TopSnapshot {
        providers: vec![
            ProviderRow {
                name: "openai".into(),
                healthy: true,
                latency_ms: 120,
            },
            ProviderRow {
                name: "local".into(),
                healthy: false,
                latency_ms: 0,
            },
        ],
        gpu: Some(GpuRow {
            util_pct: 55.0,
            temp_c: 62.0,
            vram_used_mb: 1024,
            vram_total_mb: 8192,
        }),
        queue: vec![QueueRow {
            task_id: "T-CLAUDE-1".into(),
            holder: "CURSOR".into(),
            title: "example".into(),
        }],
    };
    let frame = render_frame(&snap);
    assert!(frame.contains("susi top"));
    assert!(frame.contains("openai ok 120ms"));
    assert!(frame.contains("local down"));
    assert!(frame.contains("util=55%"));
    assert!(frame.contains("T-CLAUDE-1 [CURSOR] example"));
}

#[test]
fn tui_top_idle_queue_and_no_gpu() {
    let frame = render_frame(&TopSnapshot {
        providers: vec![],
        gpu: None,
        queue: vec![],
    });
    assert!(frame.contains("(none)"));
    assert!(frame.contains("(idle)"));
}
