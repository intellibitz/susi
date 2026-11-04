use crate::region_eligibility::{
    place, CloudTarget, PlacementDecision, RegionMeta, RegionProvenance,
};

#[test]
fn vc_201_054_rejects_unknown_region_before_payload() {
    let t = CloudTarget {
        provider: "openai".into(),
        model: "gpt".into(),
        region: None,
    };
    assert_eq!(
        place(&t, &["us-east-1"], true),
        PlacementDecision::RejectUnknownRegion
    );
}

#[test]
fn vc_201_054_rejects_disallowed_region() {
    let t = CloudTarget {
        provider: "openai".into(),
        model: "gpt".into(),
        region: Some(RegionMeta {
            region: "eu-west-1".into(),
            provenance: RegionProvenance::Verified,
        }),
    };
    assert_eq!(
        place(&t, &["us-east-1"], true),
        PlacementDecision::RejectDisallowed {
            region: "eu-west-1".into()
        }
    );
}

#[test]
fn vc_201_054_allows_attested_allowed_region() {
    let t = CloudTarget {
        provider: "openai".into(),
        model: "gpt".into(),
        region: Some(RegionMeta {
            region: "us-east-1".into(),
            provenance: RegionProvenance::OperatorAttested,
        }),
    };
    assert_eq!(
        place(&t, &["us-east-1", "us-west-2"], true),
        PlacementDecision::Allow
    );
}

#[test]
fn vc_201_054_unconstrained_allows_any_and_constrained_rejects_unverified_provenance() {
    let missing_region = CloudTarget {
        provider: "openai".into(),
        model: "gpt".into(),
        region: None,
    };
    assert_eq!(
        place(&missing_region, &["us-east-1"], false),
        PlacementDecision::Allow
    );

    let unknown_prov = CloudTarget {
        provider: "openai".into(),
        model: "gpt".into(),
        region: Some(RegionMeta {
            region: "us-east-1".into(),
            provenance: RegionProvenance::Unknown,
        }),
    };
    assert_eq!(
        place(&unknown_prov, &["us-east-1"], true),
        PlacementDecision::RejectUnknownRegion
    );

    let empty_region = CloudTarget {
        provider: "openai".into(),
        model: "gpt".into(),
        region: Some(RegionMeta {
            region: String::new(),
            provenance: RegionProvenance::Verified,
        }),
    };
    assert_eq!(
        place(&empty_region, &["us-east-1"], true),
        PlacementDecision::RejectUnknownRegion
    );
}

#[test]
fn vc_201_054_verified_provenance_allows_in_allowlist_and_rejects_empty_allowlist() {
    let verified = CloudTarget {
        provider: "anthropic".into(),
        model: "claude".into(),
        region: Some(RegionMeta {
            region: "us-west-2".into(),
            provenance: RegionProvenance::Verified,
        }),
    };
    assert_eq!(
        place(&verified, &["us-west-2"], true),
        PlacementDecision::Allow
    );
    assert_eq!(
        place(&verified, &[], true),
        PlacementDecision::RejectDisallowed {
            region: "us-west-2".into()
        }
    );
}
