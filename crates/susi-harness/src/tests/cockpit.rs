use super::*;
use crate::control::ControlState;
use crate::event::{append_event, EventKind};
use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
use crate::pricing::TokenUsage;
use std::net::SocketAddr;

fn entry(mission: &str, task: &str, micros: u64) -> LedgerEntry {
    LedgerEntry::settle(Dispatch {
        mission: mission.into(),
        task: task.into(),
        attempt: 1,
        model: "deepseek-flash".into(),
        provider: "deepseek".into(),
        usage: TokenUsage::new(1_000_000, 0, 0),
        price: ModelPrice {
            input_miss: MicroUsd::from_micros(micros),
            input_hit: MicroUsd::ZERO,
            output: MicroUsd::ZERO,
        },
        started_unix_ms: 0,
        ended_unix_ms: 1,
        outcome: Outcome::Completed,
    })
}

fn cockpit(dir: &std::path::Path) -> Cockpit {
    let ledger_path = dir.join("ledger.jsonl");
    let events_path = dir.join("events.jsonl");
    append_entry(&ledger_path, &entry("m-1", "t-1", 100)).unwrap();
    append_entry(&ledger_path, &entry("m-1", "t-2", 200)).unwrap();
    append_entry(&ledger_path, &entry("m-2", "t-3", 50)).unwrap();
    append_event(
        &events_path,
        &Event::new(1, 10, EventKind::MissionStarted, "started").with_mission("m-1"),
    )
    .unwrap();
    Cockpit {
        ledger_path,
        events_path,
        state_path: None,
        plan_path: None,
        controls_path: None,
        catalogue_path: None,
        graph_path: None,
        budget: None,
        mission: None,
    }
}

#[test]
fn cockpit_graph_appears_beside_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let cockpit = cockpit(dir.path());
    assert!(cockpit.snapshot().graph.is_empty());
    std::fs::write(
        dir.path().join("graph.jsonl"),
        "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{}}\n\
         {\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"approval\",\"payload\":{}}\n",
    )
    .unwrap();
    let snapshot = cockpit.snapshot();
    assert_eq!(snapshot.graph.messages.len(), 2);
    assert_eq!(snapshot.graph.roles, vec!["planner", "reviewer"]);
    assert!(snapshot.graph.approved());
}

#[test]
fn the_snapshot_replays_the_focused_mission_from_events_ledger_and_graph() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    cockpit.mission = Some("m-1".into());
    append_event(
        &cockpit.events_path,
        &Event::new(2, 20, EventKind::TaskDispatched, "task t-1").with_mission("m-1"),
    )
    .unwrap();
    let graph = dir.path().join("mission-graph.jsonl");
    std::fs::write(
        &graph,
        "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{}}\n",
    )
    .unwrap();
    cockpit.graph_path = Some(graph);

    let snapshot = cockpit.snapshot();
    assert_eq!(
        snapshot
            .replay
            .iter()
            .map(|step| (step.at_unix_ms, step.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "ledger:deepseek-flash"),
            (1, "ledger:deepseek-flash"),
            (10, "event:mission_started"),
            (10, "graph:planner->coders:plan"),
            (20, "event:task_dispatched"),
        ]
    );
    let ledger_costs: Vec<u64> = snapshot
        .replay
        .iter()
        .filter(|step| step.kind == "ledger:deepseek-flash")
        .map(|step| step.cost_micro_usd)
        .collect();
    assert_eq!(ledger_costs, vec![100, 200]);
}

#[test]
fn a_named_graph_supplies_roles_and_per_role_spend() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    cockpit.mission = Some("m-1".into());
    append_entry(&cockpit.ledger_path, &entry("m-1", "planner", 700)).unwrap();
    append_entry(&cockpit.ledger_path, &entry("m-2", "planner", 900)).unwrap();
    let graph = dir.path().join("mission-graph.jsonl");
    std::fs::write(
        &graph,
        "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{}}\n\
         {\"from\":\"coders\",\"to\":\"reviewer\",\"kind\":\"work\",\"payload\":{}}\n\
         {\"from\":\"reviewer\",\"to\":\"planner\",\"kind\":\"approval\",\"payload\":{}}\n",
    )
    .unwrap();
    cockpit.graph_path = Some(graph);
    let snapshot = cockpit.snapshot();
    assert_eq!(snapshot.graph.roles, vec!["coders", "planner", "reviewer"]);
    assert_eq!(
        snapshot
            .role_spend
            .iter()
            .map(|spend| spend.role.as_str())
            .collect::<Vec<_>>(),
        vec!["coders", "planner", "reviewer"]
    );
    let planner = snapshot
        .role_spend
        .iter()
        .find(|spend| spend.role == "planner")
        .unwrap();
    assert_eq!(planner.calls, 1);
    assert_eq!(planner.cost_micro_usd, 700);
    let coders = snapshot
        .role_spend
        .iter()
        .find(|spend| spend.role == "coders")
        .unwrap();
    assert_eq!(coders.calls, 0);
    assert_eq!(coders.cost_micro_usd, 0);
}

#[test]
fn a_snapshot_groups_and_sums_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = cockpit(dir.path()).snapshot();
    assert_eq!(snapshot.total_cost_micro_usd, 350);
    assert_eq!(snapshot.missions.len(), 2);
    let first = &snapshot.missions[0];
    assert_eq!(first.mission, "m-1");
    assert_eq!(first.calls, 2);
    assert_eq!(first.tasks, 2);
    assert_eq!(first.cost_micro_usd, 300);
    assert_eq!(first.state, "running");
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.workers.len(), 1);
    assert_eq!(snapshot.workers[0].calls, 3);
}

#[test]
fn a_plan_and_state_show_progress_and_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    let plan = dir.path().join("plan.json");
    std::fs::write(
        &plan,
        r#"{"mission":"m-1","tasks":[{"key":"a"},{"key":"b"},{"key":"c"}]}"#,
    )
    .unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(
        &state,
        r#"{"records":{"a":{"status":"done"},"b":{"status":"failed","reason":"no"}}}"#,
    )
    .unwrap();
    cockpit.plan_path = Some(plan);
    cockpit.state_path = Some(state);
    cockpit.budget = Some(Budget::new(MicroUsd::from_micros(1_000)));
    let snapshot = cockpit.snapshot();
    let mission = snapshot
        .missions
        .iter()
        .find(|mission| mission.mission == "m-1")
        .unwrap();
    assert_eq!(mission.planned, 3);
    assert_eq!(mission.done, 1);
    assert_eq!(mission.failed, 1);
    assert_eq!(snapshot.budget_micro_usd, Some(1_000));
    assert_eq!(snapshot.budget_remaining_micro_usd, Some(700));
    assert!(snapshot
        .warnings
        .iter()
        .any(|warning| warning.contains("failed")));
    assert!(snapshot
        .warnings
        .iter()
        .any(|warning| warning.contains("no activity")));
}

#[test]
fn a_plan_that_never_started_is_warned() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.json");
    std::fs::write(
        &plan,
        r#"{"mission":"m-9","tasks":[{"key":"a"},{"key":"b"}]}"#,
    )
    .unwrap();
    let cockpit = Cockpit {
        ledger_path: dir.path().join("ledger.jsonl"),
        events_path: dir.path().join("events.jsonl"),
        state_path: None,
        plan_path: Some(plan),
        controls_path: None,
        catalogue_path: None,
        graph_path: None,
        budget: None,
        mission: None,
    };
    let snapshot = cockpit.snapshot();
    let mission = &snapshot.missions[0];
    assert_eq!(mission.mission, "m-9");
    assert_eq!(mission.planned, 2);
    assert_eq!(mission.state, "reported");
    assert!(snapshot
        .warnings
        .iter()
        .any(|warning| warning.contains("never started")));
}

#[test]
fn an_operator_pause_is_reflected_and_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    let controls = dir.path().join("controls.jsonl");
    cockpit.controls_path = Some(controls.clone());
    control::submit(
        &controls,
        ControlCommand::Pause {
            mission_id: "m-1".into(),
        },
    )
    .unwrap();
    append_event(
        &cockpit.events_path,
        &Event::new(2, 20, EventKind::MissionPaused, "paused").with_mission("m-1"),
    )
    .unwrap();
    let snapshot = cockpit.snapshot();
    assert_eq!(snapshot.controls.len(), 1);
    assert!(ControlState::from_records(&snapshot.controls).paused);
    let mission = snapshot
        .missions
        .iter()
        .find(|mission| mission.mission == "m-1")
        .unwrap();
    assert_eq!(mission.state, "paused");
    assert!(snapshot
        .warnings
        .iter()
        .any(|warning| warning.contains("paused")));
}

#[tokio::test]
async fn the_router_serves_status_and_no_html_panel() {
    let dir = tempfile::tempdir().unwrap();
    let cockpit = Arc::new(cockpit(dir.path()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = super::serve(listener, cockpit, deny_writes()).await;
    });

    let status = get_text(address, "/api/status").await;
    assert!(status.contains("\"m-1\""));
    assert!(status.contains("350"));
    assert!(status.contains("\"workers\""));

    // Core ships no UI: the root path is not a route any more.
    let root = tokio::task::spawn_blocking(move || {
        let url = format!("http://{address}/");
        susi_http_transport::http_call("GET", &url, &[], 5, 0).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(root.status, 404);
}

/// A writer policy that admits only `Authorization: Bearer <token>`.
fn bearer(token: &'static str) -> CommandAuthorizer {
    Arc::new(move |context: &CommandAuthContext<'_>| {
        context.header("authorization") == Some(format!("Bearer {token}").as_str())
    })
}

fn deny_writes() -> CommandAuthorizer {
    Arc::new(|_: &CommandAuthContext<'_>| false)
}

async fn post_command(address: SocketAddr, body: &str, authorization: Option<&str>) -> String {
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /api/command HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut collected = Vec::new();
        let _ = stream.read_to_end(&mut collected);
        String::from_utf8_lossy(&collected).into_owned()
    })
    .await
    .unwrap()
}

/// Plan A2: an operator write without the API token is refused with 401
/// and never reaches the control log.
#[tokio::test]
async fn a_command_without_a_token_is_refused_with_401() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    let controls = dir.path().join("controls.jsonl");
    cockpit.controls_path = Some(controls.clone());
    let cockpit = Arc::new(cockpit);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = super::serve(listener, cockpit, bearer("test-token")).await;
    });
    let body = r#"{"command":"pause","mission_id":"m-1"}"#;
    for authorization in [None, Some("Bearer wrong-token"), Some("test-token")] {
        let response = post_command(address, body, authorization).await;
        assert!(response.starts_with("HTTP/1.1 401"), "{response}");
        assert!(
            response
                .to_ascii_lowercase()
                .contains("www-authenticate: bearer"),
            "{response}"
        );
    }
    assert!(control::read_records(&controls).is_empty());
}

/// Without connect info the peer is unknown and treated as non-loopback,
/// so a loopback-only policy fails closed rather than open.
#[tokio::test]
async fn an_unknown_peer_is_not_treated_as_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    let controls = dir.path().join("controls.jsonl");
    cockpit.controls_path = Some(controls.clone());
    let cockpit = Arc::new(cockpit);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let loopback_only: CommandAuthorizer =
        Arc::new(|context: &CommandAuthContext<'_>| context.peer.is_loopback());
    tokio::spawn(crate::http::serve(
        listener,
        super::router(cockpit, loopback_only),
        Peer::Withhold,
        std::future::pending(),
    ));
    let body = r#"{"command":"pause","mission_id":"m-1"}"#;
    let response = post_command(address, body, None).await;
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(control::read_records(&controls).is_empty());
}

#[tokio::test]
async fn a_command_over_http_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut cockpit = cockpit(dir.path());
    let controls = dir.path().join("controls.jsonl");
    cockpit.controls_path = Some(controls.clone());
    let cockpit = Arc::new(cockpit);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = super::serve(listener, cockpit, bearer("test-token")).await;
    });
    let body = r#"{"command":"pause","mission_id":"m-1"}"#;
    let response = post_command(address, body, Some("Bearer test-token")).await;
    assert!(response.contains("\"accepted\":true"), "{response}");
    let records = control::read_records(&controls);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].command.mission_id(), Some("m-1"));
}

#[tokio::test]
async fn the_event_route_streams_sse() {
    let dir = tempfile::tempdir().unwrap();
    let cockpit = Arc::new(cockpit(dir.path()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = super::serve(listener, cockpit, deny_writes()).await;
    });

    let text = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(b"GET /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut collected = Vec::new();
        loop {
            let mut buffer = [0u8; 1024];
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    collected.extend_from_slice(&buffer[..read]);
                    if String::from_utf8_lossy(&collected).contains("data:") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&collected).into_owned()
    })
    .await
    .unwrap();
    assert!(text.contains("text/event-stream"));
    assert!(text.contains("data:"));
}

async fn get_text(address: SocketAddr, path: &str) -> String {
    let url = format!("http://{address}{path}");
    tokio::task::spawn_blocking(move || {
        let call = susi_http_transport::http_call("GET", &url, &[], 5, 0).unwrap();
        assert_eq!(call.status, 200);
        call.into_utf8(64 * 1024).unwrap()
    })
    .await
    .unwrap()
}

#[test]
fn snapshot_reports_prompt_cache_share_and_response_cache_hits() {
    let dir = tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger.jsonl");
    let events_path = dir.path().join("events.jsonl");

    let mut hit_entry = LedgerEntry::settle(Dispatch {
        mission: "m-cache".into(),
        task: "t-1".into(),
        attempt: 1,
        model: "claude-opus".into(),
        provider: "anthropic".into(),
        usage: TokenUsage::new(10_000, 9_000, 1_500),
        price: ModelPrice {
            input_miss: MicroUsd::from_micros(5_000_000),
            input_hit: MicroUsd::from_micros(500_000),
            output: MicroUsd::from_micros(25_000_000),
        },
        started_unix_ms: 10,
        ended_unix_ms: 20,
        outcome: Outcome::Completed,
    });
    append_entry(&ledger_path, &hit_entry).unwrap();

    hit_entry.cached = true;
    hit_entry.cost = MicroUsd::ZERO;
    append_entry(&ledger_path, &hit_entry).unwrap();

    let cockpit = Cockpit {
        ledger_path,
        events_path,
        state_path: None,
        plan_path: None,
        controls_path: None,
        catalogue_path: None,
        graph_path: None,
        budget: None,
        mission: None,
    };
    let snap = cockpit.snapshot();
    assert_eq!(snap.total_input_tokens, 20_000);
    assert_eq!(snap.total_cache_hit_tokens, 18_000);
    assert_eq!(snap.total_output_tokens, 3_000);
    assert_eq!(snap.cache_share_permille, 900);
    assert_eq!(snap.cached_calls, 1);
    assert_eq!(snap.workers.len(), 1);
    assert_eq!(snap.workers[0].cache_share_permille, 900);
    assert_eq!(snap.workers[0].cached_calls, 1);
    assert_eq!(
        snap.roi.actual_cost_micro_usd + snap.roi.total_saved_micro_usd,
        snap.roi.frontier_baseline_micro_usd
    );
    assert!(snap.roi.prompt_cache_saved_micro_usd > 0);
    assert!(snap.roi.response_cache_saved_micro_usd > 0);
    assert_eq!(
        snap.workers[0].saved_micro_usd,
        snap.roi.total_saved_micro_usd
    );
}
