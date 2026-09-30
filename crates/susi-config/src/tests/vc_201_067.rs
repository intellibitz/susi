//! Environment-scoped configuration profiles (VC-201-067).

use crate::env_profiles::{EnvKind, Profile, ProfileSet};
use serde_json::json;
use std::collections::BTreeMap;

type Spec<'a> = (
    &'a str,
    EnvKind,
    &'a [&'a str],
    &'a [(&'a str, serde_json::Value)],
);

fn profiles(list: &[Spec<'_>]) -> ProfileSet {
    ProfileSet {
        profiles: list
            .iter()
            .map(|(name, env, extends, values)| {
                (
                    name.to_string(),
                    Profile {
                        env: *env,
                        extends: extends.iter().map(|s| s.to_string()).collect(),
                        values: values
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.clone()))
                            .collect(),
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn vc_201_067_dev_cannot_extend_production() {
    let set = profiles(&[
        (
            "prod",
            EnvKind::Production,
            &[],
            &[("endpoint", json!("prod.example.com"))],
        ),
        ("dev", EnvKind::Development, &["prod"], &[]),
    ]);
    let err = set.validate().unwrap_err().to_string();
    assert!(err.contains("higher-environment"), "{err}");
    // Resolution refuses too — the graph is validated before any read.
    assert!(set.resolve("dev").is_err());
}

#[test]
fn vc_201_067_shared_base_resolves_with_provenance() {
    let set = profiles(&[
        (
            "base",
            EnvKind::Shared,
            &[],
            &[("log_level", json!("info")), ("endpoint", json!("api"))],
        ),
        (
            "dev",
            EnvKind::Development,
            &["base"],
            &[("endpoint", json!("localhost"))],
        ),
    ]);
    let r = set.resolve("dev").unwrap();
    assert_eq!(r.env, EnvKind::Development);
    assert_eq!(r.values["log_level"], json!("info"));
    assert_eq!(r.values["endpoint"], json!("localhost"));
    // Inspectable: every key reports its supplying profile and env.
    assert_eq!(r.provenance["log_level"].profile, "base");
    assert_eq!(r.provenance["log_level"].env, EnvKind::Shared);
    assert_eq!(r.provenance["endpoint"].profile, "dev");
    assert_eq!(r.chain, vec!["base".to_string(), "dev".to_string()]);
}

#[test]
fn vc_201_067_dev_resolves_only_dev_and_shared_sources() {
    let set = profiles(&[
        ("base", EnvKind::Shared, &[], &[("region", json!("us"))]),
        (
            "prod",
            EnvKind::Production,
            &["base"],
            &[("key", json!("prod-secret"))],
        ),
        (
            "dev",
            EnvKind::Development,
            &["base"],
            &[("key", json!("dev-secret"))],
        ),
    ]);
    set.validate().unwrap();
    let dev = set.resolve("dev").unwrap();
    for src in dev.provenance.values() {
        assert!(
            matches!(src.env, EnvKind::Development | EnvKind::Shared),
            "dev resolved {} from {:?}",
            src.profile,
            src.env
        );
    }
    assert_eq!(dev.values["key"], json!("dev-secret"));
}

#[test]
fn vc_201_067_shared_profile_extending_production_is_not_shareable() {
    // A "shared" base that secretly pulls prod values defeats isolation.
    let set = profiles(&[
        (
            "prod",
            EnvKind::Production,
            &[],
            &[("endpoint", json!("prod"))],
        ),
        ("base", EnvKind::Shared, &["prod"], &[]),
        ("dev", EnvKind::Development, &["base"], &[]),
    ]);
    assert!(set.validate().is_err());
}

#[test]
fn vc_201_067_unknown_parent_and_cycles_fail_validation() {
    let dangling = profiles(&[("dev", EnvKind::Development, &["ghost"], &[])]);
    assert!(dangling.validate().is_err());
    let cyclic = profiles(&[
        ("a", EnvKind::Development, &["b"], &[]),
        ("b", EnvKind::Development, &["a"], &[]),
    ]);
    let err = cyclic.validate().unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn vc_201_067_production_may_consume_shared_and_lower() {
    let set = profiles(&[
        ("dev", EnvKind::Development, &[], &[("flag", json!(true))]),
        (
            "prod",
            EnvKind::Production,
            &["dev"],
            &[("endpoint", json!("p"))],
        ),
    ]);
    set.validate().unwrap();
    let prod = set.resolve("prod").unwrap();
    assert_eq!(prod.values["flag"], json!(true));
    let _m: BTreeMap<String, String> = BTreeMap::new();
}
