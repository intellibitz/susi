//! Query API (Swarm OS Bullet 48)
//!
//! Substring search over cell ids, world-model payloads, and event
//! payloads. Callers pass the stores they already hold.

use crate::event_sourcing::StoredEvent;
use crate::world_model::WorldModel;
use susi_abi::swarm::SwarmCellManifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryHit {
    pub kind: &'static str,
    pub id: String,
}

pub fn query(
    cells: &[SwarmCellManifest],
    model: &WorldModel,
    events: &[StoredEvent],
    needle: &str,
) -> Vec<QueryHit> {
    let mut hits = Vec::new();
    for cell in cells {
        if cell.cell_id.contains(needle) {
            hits.push(QueryHit {
                kind: "agent",
                id: cell.cell_id.clone(),
            });
        }
    }
    for node in model.latest_nodes() {
        if node.id.contains(needle) || node.payload.contains(needle) {
            hits.push(QueryHit {
                kind: "memory",
                id: node.id,
            });
        }
    }
    for event in events {
        let text = String::from_utf8_lossy(&event.payload);
        if text.contains(needle) {
            hits.push(QueryHit {
                kind: "event",
                id: event.seq.to_string(),
            });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_sourcing::StoredEvent;
    use crate::world_model::{NodeDraft, TypePerm, WorldModel};
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    #[test]
    fn a_needle_hits_an_agent_a_memory_payload_and_an_event() {
        let cells = vec![SwarmCellManifest {
            cell_id: "alpha-cell".into(),
            role: SwarmRole::ReflexCell,
            capabilities: Vec::new(),
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/test.sock".into(),
            trust_score: 0.1,
            last_heartbeat: 0,
        }];
        let mut model = WorldModel::new();
        model.grant("writer", "note", TypePerm::Write);
        model
            .put(
                "writer",
                NodeDraft {
                    id: "note-1".into(),
                    type_name: "note".into(),
                    namespace: "proj".into(),
                    region: "eu".into(),
                    payload: "alpha observation".into(),
                },
            )
            .unwrap();
        let events = vec![StoredEvent {
            seq: 4,
            ts: 1,
            payload: b"alpha happened".to_vec(),
        }];
        let hits = query(&cells, &model, &events, "alpha");
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().any(|hit| hit.kind == "agent"));
        assert!(hits.iter().any(|hit| hit.kind == "memory"));
        assert!(hits.iter().any(|hit| hit.kind == "event" && hit.id == "4"));
    }
}
