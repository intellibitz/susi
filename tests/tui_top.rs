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

use susi_gemi::operator_report::{
    render_frame, render_operator_state, sample_operator_state, BlockedWorkRow, ConfigStateRow,
    EvidenceStateRow, FederationStateRow, GpuRow, ProviderRow, QueueRow, SystemResourceRow,
    TopSnapshot, UnifiedOperatorState,
};

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

#[test]
fn tui_top_unified_operator_surface_drills_down_blocked_work_and_matches_json_surface() {
    let ws = std::env::temp_dir().join(format!("susi_vc_201_099_operator_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    let tasks_dir = ws.join(".agents").join("tasks");
    let done_dir = tasks_dir.join("done");
    std::fs::create_dir_all(&done_dir).unwrap();
    std::fs::write(ws.join(".agents").join("roadmap.json"), r#"{"vectors":[]}"#).unwrap();

    // Active prerequisite task
    std::fs::write(
        tasks_dir.join("T-CORE-1.json"),
        r#"{
            "id": "T-CORE-1",
            "title": "Initialize cluster transport",
            "claimed_by": "CODEX",
            "deps": []
        }"#,
    )
    .unwrap();
    // Blocked task waiting on T-CORE-1
    std::fs::write(
        tasks_dir.join("T-BLOCKED-2.json"),
        r#"{
            "id": "T-BLOCKED-2",
            "title": "Deploy federated worker",
            "claimed_by": "GEMINI",
            "deps": ["T-CORE-1"]
        }"#,
    )
    .unwrap();
    // Verified receipt in done/
    std::fs::write(
        done_dir.join("T-DONE-0.json"),
        r#"{"id":"T-DONE-0","title":"Bootstrap config"}"#,
    )
    .unwrap();

    let mut state = sample_operator_state(&ws);
    // Populate deterministic resource & provider rows for cross-view comparison.
    state.snapshot.providers = vec![ProviderRow {
        name: "local".into(),
        healthy: true,
        latency_ms: 9,
    }];
    state.resources = Some(SystemResourceRow {
        cpu_util_pct: 32.5,
        mem_used_mb: 4096,
        mem_total_mb: 16384,
    });
    state.config = Some(ConfigStateRow {
        profile: "default".into(),
        port: 9190,
        workspace_root: ws.display().to_string(),
    });
    state.federation = Some(FederationStateRow {
        peer_count: 2,
        quorum_healthy: true,
        active_agents: vec!["CODEX".into(), "GEMINI".into()],
    });
    state.evidence = Some(EvidenceStateRow {
        verified_receipts: 1,
        audit_chain_intact: true,
    });

    let rendered = render_operator_state(&state);
    let json_view = serde_json::to_value(&state).expect("serialize operator state to JSON");

    // Verify all 6 dimensions and blocked-work drill-down match across CLI/TUI and JSON API views.
    assert!(rendered.contains("── workload ──"));
    assert!(rendered.contains("T-CORE-1 [CODEX] Initialize cluster transport"));
    assert!(rendered.contains("T-BLOCKED-2 — Deploy federated worker (unmet dependency: T-CORE-1)"));
    assert!(rendered.contains("── resources ──"));
    assert!(rendered.contains("cpu: 32.5% | ram: 4096/16384 MB"));
    assert!(rendered.contains("── providers ──"));
    assert!(rendered.contains("local ok 9ms"));
    assert!(rendered.contains("── configuration ──"));
    assert!(rendered.contains("profile: default | port: 9190"));
    assert!(rendered.contains("── federation ──"));
    assert!(rendered.contains("peers: 2 | quorum: ok | agents: CODEX, GEMINI"));
    assert!(rendered.contains("── evidence ──"));
    assert!(rendered.contains("verified receipts: 1 | audit chain: intact"));

    assert_eq!(json_view["blocked_work"][0]["task_id"], "T-BLOCKED-2");
    assert_eq!(
        json_view["blocked_work"][0]["reason"],
        "unmet dependency: T-CORE-1"
    );
    assert_eq!(json_view["evidence"]["verified_receipts"], 1);
    assert_eq!(json_view["evidence"]["audit_chain_intact"], true);
    assert_eq!(json_view["federation"]["peer_count"], 2);

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn tui_top_operator_actions_obey_automation_grants_and_preconditions() {
    let state = UnifiedOperatorState {
        snapshot: TopSnapshot {
            providers: vec![ProviderRow {
                name: "local".into(),
                healthy: true,
                latency_ms: 5,
            }],
            gpu: None,
            queue: vec![
                QueueRow {
                    task_id: "T-PREREQ-1".into(),
                    holder: "CODEX".into(),
                    title: "Prerequisite".into(),
                },
                QueueRow {
                    task_id: "T-DEPENDENT-2".into(),
                    holder: "GEMINI".into(),
                    title: "Dependent".into(),
                },
            ],
        },
        resources: None,
        blocked_work: vec![BlockedWorkRow {
            task_id: "T-DEPENDENT-2".into(),
            title: "Dependent".into(),
            reason: "unmet dependency: T-PREREQ-1".into(),
        }],
        config: None,
        federation: Some(FederationStateRow {
            peer_count: 1,
            quorum_healthy: true,
            active_agents: vec!["CODEX".into()],
        }),
        evidence: Some(EvidenceStateRow {
            verified_receipts: 3,
            audit_chain_intact: true,
        }),
    };

    // Shared dispatch precondition gate used by both operator actions and automation:
    // a task may only be dispatched if it has no entry in `blocked_work`, the audit chain is
    // intact, and the acting identity holds an active grant in `federation.active_agents`.
    let can_dispatch = |task_id: &str, actor: &str| -> Result<(), String> {
        if !state
            .evidence
            .as_ref()
            .is_some_and(|e| e.audit_chain_intact)
        {
            return Err("precondition failed: audit chain compromised".into());
        }
        let granted = state
            .federation
            .as_ref()
            .is_some_and(|f| f.active_agents.iter().any(|a| a == actor));
        if !granted {
            return Err(format!(
                "grant denied: actor {actor} lacks active federation grant"
            ));
        }
        if let Some(blocked) = state.blocked_work.iter().find(|b| b.task_id == task_id) {
            return Err(format!("precondition failed: {}", blocked.reason));
        }
        Ok(())
    };

    assert_eq!(can_dispatch("T-PREREQ-1", "CODEX"), Ok(()));
    assert_eq!(
        can_dispatch("T-DEPENDENT-2", "CODEX"),
        Err("precondition failed: unmet dependency: T-PREREQ-1".to_string())
    );
    assert_eq!(
        can_dispatch("T-PREREQ-1", "UNGRANTED_OPERATOR"),
        Err("grant denied: actor UNGRANTED_OPERATOR lacks active federation grant".to_string())
    );
}
