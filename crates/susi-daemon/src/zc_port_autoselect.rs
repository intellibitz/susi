//! Auto-select free ports and publish them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortPick {
    pub service: String,
    pub port: u16,
}

#[must_use]
pub fn autoselect_ports(services: &[(String, u16)], occupied: &BTreeSet<u16>) -> Vec<PortPick> {
    let mut used = occupied.clone();
    let mut out = Vec::new();
    for (svc, pref) in services {
        let mut p = *pref;
        while !used.insert(p) {
            p = p.saturating_add(1);
        }
        out.push(PortPick {
            service: svc.clone(),
            port: p,
        });
    }
    out
}

#[cfg(test)]
mod zc_port_autoselect_tests {
    use super::*;

    #[test]
    fn zc_port_autoselect_avoids_occupied_and_publishes() {
        let mut occ = BTreeSet::new();
        occ.insert(9090);
        let picks = autoselect_ports(&[("daemon".into(), 9090), ("a2a".into(), 9091)], &occ);
        assert_eq!(picks[0].port, 9091);
        assert_eq!(picks[1].port, 9092);
    }

    #[test]
    fn zc_port_autoselect_empty_services_returns_empty_vec() {
        let picks = autoselect_ports(&[], &BTreeSet::new());
        assert!(picks.is_empty());
    }

    #[test]
    fn zc_port_autoselect_preserves_preferred_ports_when_unoccupied() {
        let picks = autoselect_ports(
            &[("gmcp".into(), 9090), ("gemi".into(), 9095)],
            &BTreeSet::new(),
        );
        assert_eq!(picks.len(), 2);
        assert_eq!(
            picks[0],
            PortPick {
                service: "gmcp".into(),
                port: 9090
            }
        );
        assert_eq!(
            picks[1],
            PortPick {
                service: "gemi".into(),
                port: 9095
            }
        );
    }

    #[test]
    fn zc_port_autoselect_resolves_duplicate_preferred_ports_sequentially() {
        let picks = autoselect_ports(
            &[("svc-a".into(), 9090), ("svc-b".into(), 9090)],
            &BTreeSet::new(),
        );
        assert_eq!(picks[0].port, 9090);
        assert_eq!(picks[1].port, 9091);
    }
}
