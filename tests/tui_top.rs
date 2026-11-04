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

use susi_gemi::operator_report::{render_frame, GpuRow, ProviderRow, QueueRow, TopSnapshot};

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

#[test]
fn tui_top_renders_vram_usage_and_temperature_metrics() {
    let frame = render_frame(&TopSnapshot {
        providers: vec![],
        gpu: Some(GpuRow {
            util_pct: 88.4,
            temp_c: 71.2,
            vram_used_mb: 4096,
            vram_total_mb: 16384,
        }),
        queue: vec![],
    });
    assert!(frame.contains("util=88% temp=71C vram=4096/16384MB"));
}

#[test]
fn tui_top_renders_multiple_queued_tasks_in_order() {
    let frame = render_frame(&TopSnapshot {
        providers: vec![],
        gpu: None,
        queue: vec![
            QueueRow {
                task_id: "T-ALPHA-1".into(),
                holder: "GEMINI".into(),
                title: "first mission".into(),
            },
            QueueRow {
                task_id: "T-BETA-2".into(),
                holder: "CLAUDE".into(),
                title: "second mission".into(),
            },
        ],
    });
    let first_pos = frame.find("T-ALPHA-1 [GEMINI] first mission").unwrap();
    let second_pos = frame.find("T-BETA-2 [CLAUDE] second mission").unwrap();
    assert!(first_pos < second_pos);
    assert!(!frame.contains("(idle)"));
}

#[test]
fn tui_top_renders_mixed_healthy_and_unhealthy_providers_accurately() {
    let frame = render_frame(&TopSnapshot {
        providers: vec![
            ProviderRow {
                name: "anthropic".into(),
                healthy: true,
                latency_ms: 85,
            },
            ProviderRow {
                name: "groq".into(),
                healthy: false,
                latency_ms: 999,
            },
        ],
        gpu: None,
        queue: vec![],
    });
    assert!(frame.contains("anthropic ok 85ms"));
    assert!(frame.contains("groq down"));
}
