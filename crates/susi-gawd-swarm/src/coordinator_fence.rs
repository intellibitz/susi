//! Fence coordinator failover during live missions (VC-201-035).
//!
//! Task leases and authoritative mission writes bind to accepted leadership
//! epochs; stale-coordinator writes after partition are rejected.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadershipEpoch {
    pub epoch: u64,
    pub coordinator: String,
}

impl LeadershipEpoch {
    /// Verifies that the leadership epoch has a positive epoch number and non-empty coordinator ID.
    #[must_use]
    pub fn verify_epoch_invariants(&self) -> bool {
        self.epoch > 0 && !self.coordinator.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionWrite {
    pub mission_id: String,
    pub epoch: u64,
    pub coordinator: String,
    pub payload: String,
}

impl MissionWrite {
    /// Verifies that a mission write carries non-empty mission, coordinator, and payload strings
    /// and a positive leadership epoch.
    #[must_use]
    pub fn verify_write_invariants(&self) -> bool {
        !self.mission_id.trim().is_empty()
            && self.epoch > 0
            && !self.coordinator.trim().is_empty()
            && !self.payload.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteVerdict {
    Accepted,
    StaleEpoch,
    WrongCoordinator,
}

#[derive(Debug, Default)]
pub struct FencedMissionStore {
    epoch: Option<LeadershipEpoch>,
    state: BTreeMap<String, String>,
}

impl FencedMissionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a new leadership epoch (failover). Must be strictly greater.
    pub fn accept_epoch(&mut self, epoch: LeadershipEpoch) -> bool {
        if let Some(cur) = &self.epoch {
            if epoch.epoch <= cur.epoch {
                return false;
            }
        }
        self.epoch = Some(epoch);
        true
    }

    #[must_use]
    pub fn current_epoch(&self) -> Option<&LeadershipEpoch> {
        self.epoch.as_ref()
    }

    #[must_use]
    pub fn mission(&self, id: &str) -> Option<&String> {
        self.state.get(id)
    }

    pub fn write(&mut self, w: MissionWrite) -> WriteVerdict {
        let Some(cur) = &self.epoch else {
            return WriteVerdict::StaleEpoch;
        };
        if w.epoch < cur.epoch {
            return WriteVerdict::StaleEpoch;
        }
        if w.epoch > cur.epoch {
            return WriteVerdict::StaleEpoch;
        }
        if w.coordinator != cur.coordinator {
            return WriteVerdict::WrongCoordinator;
        }
        self.state.insert(w.mission_id, w.payload);
        WriteVerdict::Accepted
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn accept_epoch_requires_strictly_increasing_epoch_numbers() {
        let mut store = FencedMissionStore::new();
        assert!(store.current_epoch().is_none());

        assert!(store.accept_epoch(LeadershipEpoch {
            epoch: 2,
            coordinator: "coord-a".to_string(),
        }));
        assert!(!store.accept_epoch(LeadershipEpoch {
            epoch: 2,
            coordinator: "coord-b".to_string(),
        }));
        assert!(!store.accept_epoch(LeadershipEpoch {
            epoch: 1,
            coordinator: "coord-b".to_string(),
        }));
        assert!(store.accept_epoch(LeadershipEpoch {
            epoch: 3,
            coordinator: "coord-b".to_string(),
        }));
        assert_eq!(
            store.current_epoch().map(|e| e.coordinator.as_str()),
            Some("coord-b")
        );
    }

    #[test]
    fn write_without_epoch_or_with_mismatched_epoch_returns_stale_epoch() {
        let mut store = FencedMissionStore::new();
        let w = MissionWrite {
            mission_id: "m-1".to_string(),
            epoch: 1,
            coordinator: "coord-a".to_string(),
            payload: "p".to_string(),
        };
        assert_eq!(store.write(w.clone()), WriteVerdict::StaleEpoch);

        store.accept_epoch(LeadershipEpoch {
            epoch: 2,
            coordinator: "coord-a".to_string(),
        });
        assert_eq!(store.write(w), WriteVerdict::StaleEpoch);
        assert_eq!(
            store.write(MissionWrite {
                mission_id: "m-1".to_string(),
                epoch: 3,
                coordinator: "coord-a".to_string(),
                payload: "p".to_string(),
            }),
            WriteVerdict::StaleEpoch
        );
    }

    #[test]
    fn write_with_matching_epoch_but_different_coordinator_returns_wrong_coordinator() {
        let mut store = FencedMissionStore::new();
        store.accept_epoch(LeadershipEpoch {
            epoch: 5,
            coordinator: "coord-a".to_string(),
        });
        let verdict = store.write(MissionWrite {
            mission_id: "m-1".to_string(),
            epoch: 5,
            coordinator: "coord-impostor".to_string(),
            payload: "bad".to_string(),
        });
        assert_eq!(verdict, WriteVerdict::WrongCoordinator);
        assert!(store.mission("m-1").is_none());
    }

    #[test]
    fn write_with_matching_epoch_and_coordinator_persists_mission_payload() {
        let mut store = FencedMissionStore::new();
        store.accept_epoch(LeadershipEpoch {
            epoch: 7,
            coordinator: "coord-a".to_string(),
        });
        let verdict = store.write(MissionWrite {
            mission_id: "m-7".to_string(),
            epoch: 7,
            coordinator: "coord-a".to_string(),
            payload: "authoritative-state".to_string(),
        });
        assert_eq!(verdict, WriteVerdict::Accepted);
        assert_eq!(
            store.mission("m-7").map(String::as_str),
            Some("authoritative-state")
        );
    }

    #[test]
    fn leadership_epoch_and_mission_write_serde_json_roundtrip() {
        let ep = LeadershipEpoch {
            epoch: 9,
            coordinator: "coord-nine".to_string(),
        };
        let ep_json = serde_json::to_string(&ep).expect("serialize LeadershipEpoch");
        let ep_back: LeadershipEpoch =
            serde_json::from_str(&ep_json).expect("deserialize LeadershipEpoch");
        assert_eq!(ep_back, ep);

        let mw = MissionWrite {
            mission_id: "m-9".to_string(),
            epoch: 9,
            coordinator: "coord-nine".to_string(),
            payload: "state-9".to_string(),
        };
        let mw_json = serde_json::to_string(&mw).expect("serialize MissionWrite");
        let mw_back: MissionWrite =
            serde_json::from_str(&mw_json).expect("deserialize MissionWrite");
        assert_eq!(mw_back, mw);

        assert!(ep.verify_epoch_invariants());
        assert!(mw.verify_write_invariants());
        assert!(!LeadershipEpoch {
            epoch: 0,
            coordinator: "coord".to_string(),
        }
        .verify_epoch_invariants());
        assert!(!MissionWrite {
            mission_id: "   ".to_string(),
            epoch: 1,
            coordinator: "coord".to_string(),
            payload: "p".to_string(),
        }
        .verify_write_invariants());
    }
}
