//! Mastery verification for VC-201-067: environment-scoped configuration
//! profiles with inspectable inheritance.
//!
//! The claim under test is not "a profile struct resolves" — the cited
//! `vc_201_067` tests show the happy path. The distinguishing properties are
//! that the documented precedence actually holds under multi-parent
//! inheritance and that a dev profile cannot reach production values through
//! any ambient route, including transitive ones.

use crate::env_profiles::{EnvKind, Profile, ProfileSet};
use serde_json::json;
use std::collections::BTreeMap;

fn profile(env: EnvKind, extends: &[&str], values: &[(&str, serde_json::Value)]) -> Profile {
    Profile {
        env,
        extends: extends.iter().map(|s| s.to_string()).collect(),
        values: values
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    }
}

fn set(list: &[(&str, Profile)]) -> ProfileSet {
    ProfileSet {
        profiles: list
            .iter()
            .map(|(n, p)| (n.to_string(), p.clone()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// Falsification: the doc contract says "parents applied in order, earlier
/// wins ties among parents". `apply` inserts each parent's values into the
/// accumulator in order, so the *later* parent overwrites the earlier —
/// the resolved value contradicts the documented precedence, and an
/// operator relying on documented ordering silently gets the wrong parent's
/// endpoint.
#[test]
fn vc_201_067_mastery_later_parent_wins_despite_documented_earlier_wins() {
    let s = set(&[
        (
            "early",
            profile(
                EnvKind::Shared,
                &[],
                &[("endpoint", json!("early.example"))],
            ),
        ),
        (
            "late",
            profile(EnvKind::Shared, &[], &[("endpoint", json!("late.example"))]),
        ),
        (
            "dev",
            profile(EnvKind::Development, &["early", "late"], &[]),
        ),
    ]);
    let r = s.resolve("dev").unwrap();
    assert_eq!(
        r.values["endpoint"],
        json!("late.example"),
        "the later parent wins ties — the documented 'earlier wins' is false"
    );
    assert_eq!(r.provenance["endpoint"].profile, "late");
}

/// Transitive falsification attempt: a dev profile cannot reach production
/// values indirectly — dev → shared-mid → prod is refused because validation
/// runs on the whole graph before resolution.
#[test]
fn vc_201_067_mastery_transitive_route_to_production_is_refused() {
    let s = set(&[
        (
            "prod",
            profile(
                EnvKind::Production,
                &[],
                &[("endpoint", json!("prod.example"))],
            ),
        ),
        ("mid", profile(EnvKind::Shared, &["prod"], &[])),
        ("dev", profile(EnvKind::Development, &["mid"], &[])),
    ]);
    let err = s.resolve("dev").unwrap_err().to_string();
    assert!(
        err.contains("higher-environment"),
        "transitive dev→shared→prod route must be refused: {err}"
    );
}

/// What does hold: child overrides parents, provenance names the final
/// supplier, cycles and unknown parents are refused before resolution, and
/// production may inherit from shared.
#[test]
fn vc_201_067_mastery_child_overrides_provenance_cycles_and_unknown() {
    let s = set(&[
        (
            "shared",
            profile(
                EnvKind::Shared,
                &[],
                &[("a", json!("base")), ("b", json!("b0"))],
            ),
        ),
        (
            "prod",
            profile(EnvKind::Production, &["shared"], &[("a", json!("prod-a"))]),
        ),
    ]);
    let r = s.resolve("prod").unwrap();
    assert_eq!(r.values["a"], json!("prod-a"), "child wins over parent");
    assert_eq!(r.provenance["a"].profile, "prod");
    assert_eq!(r.provenance["b"].profile, "shared");
    assert_eq!(r.chain, vec!["shared".to_string(), "prod".to_string()]);

    let cyc = set(&[
        ("x", profile(EnvKind::Shared, &["y"], &[])),
        ("y", profile(EnvKind::Shared, &["x"], &[])),
    ]);
    assert!(cyc.validate().is_err(), "inheritance cycle refused");

    let unknown = set(&[("z", profile(EnvKind::Shared, &["ghost"], &[]))]);
    assert!(
        unknown.validate().is_err(),
        "extends unknown profile refused"
    );
}
