//! Mastery verification for VC-202-001: provider credential inventory and
//! liveness scouting.
//!
//! The mastery target is a *liveness* inventory: every configured credential
//! enumerated, probed with the cheapest possible call, classified
//! live/expired/unauthorized/rate-limited/unknown with evidence recorded.
//! What exists today is enumeration of *sources* and *env-var presence* —
//! honest bookkeeping that never answers "does this key work". These tests
//! pin down exactly that boundary so the refutation
//! (.agents/roadmap-verdicts/VC-202-001.json) is executable, not narrative.

use crate::cloud::{known_cloud_vendors, list_api_key_status};
use crate::zc_key_sources::{discover, SourceKind, SourceProbe};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct FakeProbe {
    env: HashMap<String, String>,
    files: HashMap<PathBuf, String>,
    home: PathBuf,
}

impl SourceProbe for FakeProbe {
    fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }
    fn read_file(&self, path: &Path) -> Option<String> {
        self.files.get(path).cloned()
    }
    fn workspace_dir(&self) -> PathBuf {
        PathBuf::from("/nonexistent-workspace")
    }
    fn home(&self) -> PathBuf {
        self.home.clone()
    }
    fn command_stdout(&self, _program: &str, _args: &[&str]) -> Option<String> {
        None
    }
}

/// What does hold: `list_api_key_status` enumerates every known vendor's
/// env-var slot exactly once. What it reports is *presence* — the bool says
/// a value is registered under the name, nothing about whether the
/// credential authenticates. The record carries no liveness field because
/// no liveness probe exists to populate one.
#[test]
fn vc_202_001_mastery_inventory_is_env_presence_not_credential_liveness() {
    let rows = list_api_key_status();
    let vendors = known_cloud_vendors();
    assert_eq!(
        rows.len(),
        vendors.len(),
        "every known vendor gets exactly one status row"
    );
    for ((vendor, env, present), (want_vendor, want_env)) in rows.iter().zip(vendors.iter()) {
        assert_eq!(vendor, want_vendor);
        assert_eq!(env, want_env);
        let _ = present; // presence only — never a liveness verdict
    }
}

/// What does hold: `discover` enumerates every place a credential already
/// lives (env, workspace .env, cloud CLI stores) through an injected probe,
/// so the inventory side of the target is partially real. What is missing:
/// each `KeySource` records kind/label/alias/preview/secret and nothing
/// else — no probe result, no live/expired/unauthorized/rate-limited
/// classification, no evidence timestamp. Two discovered credentials are
/// indistinguishable whether one is revoked and the other fresh.
#[test]
fn vc_202_001_mastery_source_discovery_enumerates_but_never_classifies() {
    let home = PathBuf::from("/nonexistent-home");
    let mut probe = FakeProbe {
        env: HashMap::from([
            ("OPENAI_API_KEY".to_string(), "sk-live-maybe".to_string()),
            ("DEEPSEEK_API_KEY".to_string(), "sk-dead".to_string()),
        ]),
        files: HashMap::from([(
            home.join(".aws").join("credentials"),
            "[default]\naws_access_key_id = AKIAEXAMPLE\n".to_string(),
        )]),
        home,
    };
    let found = discover(&probe);
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].alias, "openai");
    assert_eq!(found[0].kind, SourceKind::Environment);
    assert_eq!(found[1].alias, "deepseek");
    assert_eq!(found[2].alias, "aws");
    assert_eq!(found[2].kind, SourceKind::AwsCredentials);
    // The inventory answerable today stops at "a credential exists here" —
    // asserting a liveness field would not compile, because none exists.
    for source in &found {
        assert!(!source.label.is_empty());
        assert!(!source.preview.is_empty());
    }
    probe.env.clear();
    assert_eq!(discover(&probe).len(), 1, "file sources still enumerate");
}
