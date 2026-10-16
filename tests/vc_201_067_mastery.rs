#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-067: Separate workspace and environment configuration profiles.
//!
//! Mastery criteria:
//! 1. File loading: `ProfileSet::load` loads and validates JSON profiles;
//!    `ProfileSet::load_workspace_or_default` discovers workspace profiles in `.susi/profiles.json`.
//! 2. Direct environment isolation: development cannot extend production or staging directly.
//! 3. Transitive environment isolation: development cannot reach production transitively through shared parents.
//! 4. Cycle and unknown parent detection: cyclic inheritance or nonexistent parents fail validation.
//! 5. Multi-parent precedence: earlier-listed parent wins key collisions among parents.
//! 6. Child overrides: child profile explicit values override all parent values.
//! 7. Inspectable provenance: resolved configuration accurately records each key's supplying profile,
//!    environment kind, and the full ordered inheritance chain.

use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use susi_config::env_profiles::{EnvKind, Profile, ProfileSet};

#[test]
fn vc_201_067_mastery() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ws_dir =
        std::env::temp_dir().join(format!("susi-vc201067-{}-{}", std::process::id(), nanos));
    let _ = fs::remove_dir_all(&ws_dir);
    let susi_dir = ws_dir.join(".susi");
    fs::create_dir_all(&susi_dir).expect("create .susi dir");

    // 1. Initially absent workspace profiles returns Ok(None)
    let absent = ProfileSet::load_workspace_or_default(&ws_dir).expect("load absent profiles");
    assert!(absent.is_none(), "absent profiles.json must return None");

    // Construct valid profile set:
    // - shared_base (Shared): common api timeout, region
    // - shared_secondary (Shared): region collision (tests earlier-wins)
    // - dev (Development): extends [shared_base, shared_secondary], overrides api_timeout
    // - prod (Production): extends [shared_base], sets prod credentials
    let mut profiles = BTreeMap::new();

    let mut shared_base_values = BTreeMap::new();
    shared_base_values.insert("api_timeout_ms".into(), json!(5000));
    shared_base_values.insert("region".into(), json!("us-east-1"));
    shared_base_values.insert("cluster_name".into(), json!("susi-shared"));
    profiles.insert(
        "shared_base".into(),
        Profile {
            env: EnvKind::Shared,
            extends: vec![],
            values: shared_base_values,
        },
    );

    let mut shared_secondary_values = BTreeMap::new();
    shared_secondary_values.insert("region".into(), json!("eu-west-1"));
    shared_secondary_values.insert("secondary_feature".into(), json!(true));
    profiles.insert(
        "shared_secondary".into(),
        Profile {
            env: EnvKind::Shared,
            extends: vec![],
            values: shared_secondary_values,
        },
    );

    let mut dev_values = BTreeMap::new();
    dev_values.insert("api_timeout_ms".into(), json!(1000));
    dev_values.insert("debug_mode".into(), json!(true));
    profiles.insert(
        "dev".into(),
        Profile {
            env: EnvKind::Development,
            extends: vec!["shared_base".into(), "shared_secondary".into()],
            values: dev_values,
        },
    );

    let mut prod_values = BTreeMap::new();
    prod_values.insert("prod_db_url".into(), json!("postgres://prod-db:5432/main"));
    prod_values.insert("region".into(), json!("us-east-1-prod"));
    profiles.insert(
        "prod".into(),
        Profile {
            env: EnvKind::Production,
            extends: vec!["shared_base".into()],
            values: prod_values,
        },
    );

    let profile_set = ProfileSet { profiles };

    // Persist to workspace .susi/profiles.json
    let profile_json = serde_json::to_string_pretty(&profile_set).expect("serialize profiles");
    let profile_path = susi_dir.join("profiles.json");
    fs::write(&profile_path, profile_json).expect("write profiles.json");

    // 2. Test ProfileSet::load and load_workspace_or_default
    let loaded = ProfileSet::load(&profile_path).expect("load from path");
    assert_eq!(loaded, profile_set);

    let discovered = ProfileSet::load_workspace_or_default(&ws_dir)
        .expect("load from workspace")
        .expect("discovered profile set");
    assert_eq!(discovered, profile_set);

    // 3. Test resolution of dev profile
    let resolved_dev = discovered.resolve("dev").expect("resolve dev profile");
    assert_eq!(resolved_dev.env, EnvKind::Development);

    // Child value overrides parent value
    assert_eq!(
        resolved_dev.values.get("api_timeout_ms"),
        Some(&json!(1000)),
        "child value must override parent value"
    );
    assert_eq!(
        resolved_dev
            .provenance
            .get("api_timeout_ms")
            .map(|s| s.profile.as_str()),
        Some("dev"),
        "provenance for child-overridden value must be child profile"
    );

    // Multi-parent earlier-wins tie breaking:
    // dev extends ["shared_base", "shared_secondary"], both define "region".
    // shared_base was listed earlier, so shared_base's region ("us-east-1") must win over shared_secondary's ("eu-west-1").
    assert_eq!(
        resolved_dev.values.get("region"),
        Some(&json!("us-east-1")),
        "earlier-listed parent must win ties on collision"
    );
    assert_eq!(
        resolved_dev
            .provenance
            .get("region")
            .map(|s| s.profile.as_str()),
        Some("shared_base"),
        "provenance for earlier parent tie winner must be shared_base"
    );

    // Non-colliding parent value is inherited
    assert_eq!(
        resolved_dev.values.get("secondary_feature"),
        Some(&json!(true))
    );
    assert_eq!(
        resolved_dev
            .provenance
            .get("secondary_feature")
            .map(|s| s.profile.as_str()),
        Some("shared_secondary")
    );

    // Full inheritance chain
    assert_eq!(
        resolved_dev.chain,
        vec!["shared_secondary", "shared_base", "dev"]
    );

    // 4. Direct environment isolation: dev cannot extend prod
    let mut invalid_direct = profile_set.clone();
    invalid_direct.profiles.insert(
        "dev_illegal_direct".into(),
        Profile {
            env: EnvKind::Development,
            extends: vec!["prod".into()],
            values: BTreeMap::new(),
        },
    );
    let direct_err = invalid_direct
        .validate()
        .expect_err("direct dev->prod inheritance must fail");
    assert!(
        direct_err.to_string().contains("higher-environment"),
        "direct lower-to-higher environment inheritance must be rejected: {direct_err}"
    );

    // 5. Transitive environment isolation: dev -> shared_bridge -> prod
    let mut invalid_transitive = profile_set.clone();
    invalid_transitive.profiles.insert(
        "shared_bridge".into(),
        Profile {
            env: EnvKind::Shared,
            extends: vec!["prod".into()],
            values: BTreeMap::new(),
        },
    );
    invalid_transitive.profiles.insert(
        "dev_illegal_transitive".into(),
        Profile {
            env: EnvKind::Development,
            extends: vec!["shared_bridge".into()],
            values: BTreeMap::new(),
        },
    );
    let transitive_err = invalid_transitive
        .validate()
        .expect_err("transitive dev->shared->prod inheritance must fail");
    assert!(
        transitive_err.to_string().contains("higher-environment"),
        "transitive lower-to-higher environment inheritance must be rejected: {transitive_err}"
    );

    // 6. Cyclic inheritance rejection
    let mut cyclic = ProfileSet::default();
    cyclic.profiles.insert(
        "cycle_a".into(),
        Profile {
            env: EnvKind::Shared,
            extends: vec!["cycle_b".into()],
            values: BTreeMap::new(),
        },
    );
    cyclic.profiles.insert(
        "cycle_b".into(),
        Profile {
            env: EnvKind::Shared,
            extends: vec!["cycle_a".into()],
            values: BTreeMap::new(),
        },
    );
    let cycle_err = cyclic.validate().expect_err("cyclic inheritance must fail");
    assert!(
        cycle_err.to_string().contains("cycle"),
        "cyclic inheritance must be rejected: {cycle_err}"
    );

    // 7. Unknown parent rejection
    let mut unknown = ProfileSet::default();
    unknown.profiles.insert(
        "orphan".into(),
        Profile {
            env: EnvKind::Development,
            extends: vec!["nonexistent_parent".into()],
            values: BTreeMap::new(),
        },
    );
    let unknown_err = unknown.validate().expect_err("unknown parent must fail");
    assert!(
        unknown_err.to_string().contains("unknown profile"),
        "unknown parent profile must be rejected: {unknown_err}"
    );

    let _ = fs::remove_dir_all(&ws_dir);
}
