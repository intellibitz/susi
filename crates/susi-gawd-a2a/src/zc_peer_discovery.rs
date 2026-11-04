//! Discover LAN and localhost A2A peers with fingerprint confirmation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCandidate {
    pub url: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAdmission {
    pub admitted: bool,
    pub reason: String,
}

/// Admit a peer only after the human confirms the fingerprint.
#[must_use]
pub fn admit_peer(candidate: &PeerCandidate, confirmed_fingerprint: Option<&str>) -> PeerAdmission {
    match confirmed_fingerprint {
        Some(fp) if fp == candidate.fingerprint => PeerAdmission {
            admitted: true,
            reason: "fingerprint confirmed".into(),
        },
        Some(_) => PeerAdmission {
            admitted: false,
            reason: "fingerprint mismatch".into(),
        },
        None => PeerAdmission {
            admitted: false,
            reason: "awaiting fingerprint confirmation".into(),
        },
    }
}

#[cfg(test)]
mod zc_peer_discovery_tests {
    use super::*;

    #[test]
    fn zc_peer_discovery_requires_fingerprint_confirm() {
        let c = PeerCandidate {
            url: "http://127.0.0.1:9094".into(),
            fingerprint: "abc".into(),
        };
        assert!(!admit_peer(&c, None).admitted);
        assert!(admit_peer(&c, Some("abc")).admitted);
        assert!(!admit_peer(&c, Some("nope")).admitted);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod contract_tests {
    use super::*;

    fn candidate(fingerprint: &str) -> PeerCandidate {
        PeerCandidate {
            url: "http://127.0.0.1:9094".into(),
            fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn each_admission_outcome_carries_its_exact_reason() {
        let c = candidate("abc123");
        assert_eq!(
            admit_peer(&c, Some("abc123")),
            PeerAdmission {
                admitted: true,
                reason: "fingerprint confirmed".into()
            }
        );
        assert_eq!(
            admit_peer(&c, Some("zzz")),
            PeerAdmission {
                admitted: false,
                reason: "fingerprint mismatch".into()
            }
        );
        assert_eq!(
            admit_peer(&c, None),
            PeerAdmission {
                admitted: false,
                reason: "awaiting fingerprint confirmation".into()
            }
        );
    }

    #[test]
    fn fingerprint_comparison_is_exact_and_never_fuzzy() {
        let c = candidate("abc123");
        for confirmed in [
            "ABC123", "Abc123", " abc123", "abc123 ", "abc12", "abc1234", "abc123\n", "",
        ] {
            let result = admit_peer(&c, Some(confirmed));
            assert!(!result.admitted, "{confirmed:?}");
            assert_eq!(result.reason, "fingerprint mismatch", "{confirmed:?}");
        }
    }

    #[test]
    fn admission_depends_only_on_the_fingerprint_not_the_url() {
        let a = PeerCandidate {
            url: "http://10.0.0.1:1".into(),
            fingerprint: "fp".into(),
        };
        let b = PeerCandidate {
            url: "http://evil.example:9".into(),
            fingerprint: "fp".into(),
        };
        assert_eq!(admit_peer(&a, Some("fp")), admit_peer(&b, Some("fp")));
        assert_eq!(admit_peer(&a, None), admit_peer(&b, None));
    }

    #[test]
    fn candidate_and_admission_round_trip_through_json_with_stable_shape() {
        let c = candidate("fp-1");
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"url": "http://127.0.0.1:9094", "fingerprint": "fp-1"})
        );
        assert_eq!(serde_json::from_value::<PeerCandidate>(json).unwrap(), c);

        let admission = admit_peer(&c, Some("fp-1"));
        let json = serde_json::to_value(&admission).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"admitted": true, "reason": "fingerprint confirmed"})
        );
        assert_eq!(
            serde_json::from_value::<PeerAdmission>(json).unwrap(),
            admission
        );
    }

    #[test]
    fn incomplete_or_mistyped_json_is_not_a_candidate_or_admission() {
        assert!(serde_json::from_value::<PeerCandidate>(serde_json::json!({"url": "u"})).is_err());
        assert!(
            serde_json::from_value::<PeerCandidate>(serde_json::json!({"fingerprint": "f"}))
                .is_err()
        );
        assert!(serde_json::from_value::<PeerAdmission>(
            serde_json::json!({"admitted": "yes", "reason": "r"})
        )
        .is_err());
        assert!(
            serde_json::from_value::<PeerAdmission>(serde_json::json!({"admitted": true})).is_err()
        );
    }
}
