//! The cockpit.
//!
//! A read-only status surface over what the harness already writes: the ledger
//! (spend per mission) and the event feed (what happened). It recomputes on
//! every request, so there is no second source of truth to drift; the panel
//! receives a snapshot over SSE once a second (with a slow HTTP poll as a
//! fallback).

use crate::event::{read_events, Event};
use crate::ledger::read_entries;
use crate::money::MicroUsd;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Where the cockpit reads from.
#[derive(Clone)]
pub struct Cockpit {
    /// Ledger the gateway writes.
    pub ledger_path: PathBuf,
    /// Event feed the supervisor or scheduler writes.
    pub events_path: PathBuf,
}

/// Spend for one mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionSpend {
    /// Mission id.
    pub mission: String,
    /// Priced calls.
    pub calls: usize,
    /// Distinct tasks that made calls.
    pub tasks: usize,
    /// Total spend in micro-USD.
    pub cost_micro_usd: u64,
}

/// One status snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    /// Total priced spend in micro-USD.
    pub total_cost_micro_usd: u64,
    /// Per-mission spend, ordered by mission id.
    pub missions: Vec<MissionSpend>,
    /// The most recent events, oldest first.
    pub events: Vec<Event>,
}

impl Cockpit {
    /// Read the ledger and feed and build a snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let entries = read_entries(&self.ledger_path).unwrap_or_default();
        let mut by_mission: BTreeMap<String, (usize, BTreeSet<String>, MicroUsd)> = BTreeMap::new();
        let mut total = MicroUsd::ZERO;
        for entry in &entries {
            total = total
                .checked_add(entry.cost)
                .unwrap_or(MicroUsd::from_micros(u64::MAX));
            let slot = by_mission.entry(entry.mission.clone()).or_default();
            slot.0 += 1;
            slot.1.insert(entry.task.clone());
            slot.2 = slot
                .2
                .checked_add(entry.cost)
                .unwrap_or(MicroUsd::from_micros(u64::MAX));
        }
        let missions = by_mission
            .into_iter()
            .map(|(mission, (calls, tasks, cost))| MissionSpend {
                mission,
                calls,
                tasks: tasks.len(),
                cost_micro_usd: cost.micros(),
            })
            .collect();
        let mut events = read_events(&self.events_path).unwrap_or_default();
        if events.len() > 50 {
            events = events.split_off(events.len() - 50);
        }
        Snapshot {
            total_cost_micro_usd: total.micros(),
            missions,
            events,
        }
    }
}

/// Build the cockpit router.
pub fn router(cockpit: Arc<Cockpit>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/health", get(health))
        .with_state(cockpit)
}

/// Serve the cockpit on an already-bound listener.
///
/// # Errors
///
/// Returns the listener's I/O error when the server stops.
pub async fn serve(
    listener: tokio::net::TcpListener,
    cockpit: Arc<Cockpit>,
) -> std::io::Result<()> {
    axum::serve(listener, router(cockpit)).await
}

async fn index() -> Html<&'static str> {
    Html(include_str!("panel.html"))
}

async fn health() -> &'static str {
    "ok"
}

async fn status(State(cockpit): State<Arc<Cockpit>>) -> Json<Snapshot> {
    Json(cockpit.snapshot())
}

async fn events(
    State(cockpit): State<Arc<Cockpit>>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let stream =
        futures::stream::unfold((cockpit, interval), |(cockpit, mut interval)| async move {
            interval.tick().await;
            let data = serde_json::to_string(&cockpit.snapshot()).unwrap_or_default();
            Some((
                Ok::<_, std::convert::Infallible>(SseEvent::default().data(data)),
                (cockpit, interval),
            ))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{append_event, EventKind};
    use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
    use crate::pricing::{ModelPrice, TokenUsage};
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
        }
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
        assert_eq!(snapshot.events.len(), 1);
    }

    #[tokio::test]
    async fn the_router_serves_status_and_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let cockpit = Arc::new(cockpit(dir.path()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, super::router(cockpit)).await;
        });

        let status = get_text(address, "/api/status").await;
        assert!(status.contains("\"m-1\""));
        assert!(status.contains("350"));

        let panel = get_text(address, "/").await;
        assert!(panel.contains("susi cockpit"));
    }

    #[tokio::test]
    async fn the_event_route_streams_sse() {
        let dir = tempfile::tempdir().unwrap();
        let cockpit = Arc::new(cockpit(dir.path()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, super::router(cockpit)).await;
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
}
