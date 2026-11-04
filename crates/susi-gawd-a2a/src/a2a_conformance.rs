//! A2A conformance suite for the server (minimal contract checks).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceCheck {
    pub name: String,
    pub ok: bool,
}

/// Run local conformance assertions against advertised capabilities.
#[must_use]
pub fn a2a_conformance(
    has_agent_card: bool,
    has_jsonrpc: bool,
    has_sse: bool,
) -> Vec<ConformanceCheck> {
    vec![
        ConformanceCheck {
            name: "agent_card".into(),
            ok: has_agent_card,
        },
        ConformanceCheck {
            name: "jsonrpc".into(),
            ok: has_jsonrpc,
        },
        ConformanceCheck {
            name: "sse".into(),
            ok: has_sse,
        },
    ]
}

#[must_use]
pub fn all_ok(checks: &[ConformanceCheck]) -> bool {
    checks.iter().all(|c| c.ok)
}

#[cfg(test)]
mod a2a_conformance_tests {
    use super::*;

    #[test]
    fn a2a_conformance_requires_card_rpc_sse() {
        let ok = a2a_conformance(true, true, true);
        assert!(all_ok(&ok));
        assert!(!all_ok(&a2a_conformance(true, true, false)));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod contract_tests {
    use super::*;

    #[test]
    fn every_capability_combination_reports_each_flag_under_its_own_name() {
        for bits in 0u8..8 {
            let (card, rpc, sse) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            let checks = a2a_conformance(card, rpc, sse);
            let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["agent_card", "jsonrpc", "sse"], "{bits:03b}");
            let flags: Vec<bool> = checks.iter().map(|c| c.ok).collect();
            assert_eq!(flags, [card, rpc, sse], "{bits:03b}");
            assert_eq!(all_ok(&checks), card && rpc && sse, "{bits:03b}");
        }
    }

    #[test]
    fn a_single_missing_capability_fails_the_whole_suite() {
        assert!(all_ok(&a2a_conformance(true, true, true)));
        assert!(!all_ok(&a2a_conformance(false, true, true)));
        assert!(!all_ok(&a2a_conformance(true, false, true)));
        assert!(!all_ok(&a2a_conformance(true, true, false)));
        assert!(!all_ok(&a2a_conformance(false, false, false)));
    }

    #[test]
    fn an_empty_check_list_is_vacuously_ok() {
        // Callers must pass the checks produced by `a2a_conformance`, which
        // always yields three; an empty list proves nothing.
        assert!(all_ok(&[]));
        assert_eq!(a2a_conformance(true, true, true).len(), 3);
    }

    #[test]
    fn all_ok_considers_every_check_including_the_last() {
        let mut checks = a2a_conformance(true, true, true);
        checks.push(ConformanceCheck {
            name: "extra".into(),
            ok: false,
        });
        assert!(!all_ok(&checks));
        checks.last_mut().unwrap().ok = true;
        assert!(all_ok(&checks));
    }

    #[test]
    fn conformance_checks_round_trip_through_json_with_stable_shape() {
        let check = ConformanceCheck {
            name: "sse".into(),
            ok: false,
        };
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json, serde_json::json!({"name": "sse", "ok": false}));
        assert_eq!(
            serde_json::from_value::<ConformanceCheck>(json).unwrap(),
            check
        );

        let suite = a2a_conformance(true, false, true);
        let json = serde_json::to_string(&suite).unwrap();
        let back: Vec<ConformanceCheck> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, suite);
        assert!(
            serde_json::from_value::<ConformanceCheck>(serde_json::json!({"name": "x"})).is_err()
        );
        assert!(serde_json::from_value::<ConformanceCheck>(
            serde_json::json!({"name": "x", "ok": "true"})
        )
        .is_err());
    }
}
