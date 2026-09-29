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
