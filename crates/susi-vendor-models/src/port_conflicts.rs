//! Detect and resolve local engine port conflicts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnginePortWant {
    pub engine: String,
    pub preferred_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortAssignment {
    pub engine: String,
    pub port: u16,
    pub remapped: bool,
}

/// Assign ports: first claim wins preferred; conflicts get the next free port.
#[must_use]
pub fn resolve_port_conflicts(
    wants: &[EnginePortWant],
    occupied: &BTreeSet<u16>,
) -> Vec<PortAssignment> {
    let mut used = occupied.clone();
    let mut out = Vec::new();
    for w in wants {
        if used.insert(w.preferred_port) {
            out.push(PortAssignment {
                engine: w.engine.clone(),
                port: w.preferred_port,
                remapped: false,
            });
        } else {
            let mut p = w.preferred_port.saturating_add(1);
            while !used.insert(p) {
                p = p.saturating_add(1);
            }
            out.push(PortAssignment {
                engine: w.engine.clone(),
                port: p,
                remapped: true,
            });
        }
    }
    out
}

/// Persist assignments as env-style map for SUSI config overlays.
#[must_use]
pub fn assignments_to_env(assigns: &[PortAssignment]) -> BTreeMap<String, String> {
    assigns
        .iter()
        .map(|a| {
            (
                format!("SUSI_ENGINE_PORT_{}", a.engine.to_ascii_uppercase()),
                a.port.to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
mod port_conflicts_tests {
    use super::*;

    #[test]
    fn port_conflicts_remaps_second_engine_off_8080() {
        let wants = vec![
            EnginePortWant {
                engine: "llama".into(),
                preferred_port: 8080,
            },
            EnginePortWant {
                engine: "localai".into(),
                preferred_port: 8080,
            },
        ];
        let got = resolve_port_conflicts(&wants, &BTreeSet::new());
        assert_eq!(got[0].port, 8080);
        assert!(!got[0].remapped);
        assert_eq!(got[1].port, 8081);
        assert!(got[1].remapped);
        let env = assignments_to_env(&got);
        assert_eq!(
            env.get("SUSI_ENGINE_PORT_LOCALAI").map(String::as_str),
            Some("8081")
        );
    }

    #[test]
    fn port_conflicts_respects_already_occupied() {
        let mut occ = BTreeSet::new();
        occ.insert(8080);
        let wants = vec![EnginePortWant {
            engine: "llama".into(),
            preferred_port: 8080,
        }];
        let got = resolve_port_conflicts(&wants, &occ);
        assert_eq!(got[0].port, 8081);
        assert!(got[0].remapped);
    }
}
