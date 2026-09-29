//! Knowledge-base delivery through the signed catalog channel
//! (`crate::signed_catalog`): a KB update is a `CatalogBundle` on the
//! `ecosystem-kb` channel whose payload is a serialized
//! [`KnowledgeBase`]. [`install_kb`] verifies the signature, refuses
//! downgrades/replays, keeps the previous payload as rollback, then explodes
//! the payload into the git-friendly entity-file layout under
//! `<dir>/store/` that [`crate::eco_store::load_dir`] reads.
//!
//! Layout under the channel dir:
//!
//! ```text
//! catalog.json / catalog.prev.json      — signed-catalog payload copies
//! catalog.manifest.json / .prev         — version bookkeeping
//! store/<entity-id>.json                — the exploded store
//! ```
use crate::eco_schema::{validate, KnowledgeBase};
use crate::signed_catalog::{self, CatalogBundle, InstallOutcome};
use ed25519_dalek::{SigningKey, VerifyingKey};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult, ResultExt as Context};

/// Catalog channel the knowledge base arrives on.
pub const KB_CHANNEL: &str = "ecosystem-kb";
/// Subdirectory of the channel dir holding the exploded entity files.
pub const STORE_SUBDIR: &str = "store";

/// Sign a knowledge base for delivery. `version` must exceed the currently
/// installed catalog version for [`install_kb`] to accept it.
#[must_use]
pub fn bundle_kb(
    kb: &KnowledgeBase,
    version: u64,
    key: &SigningKey,
    key_id: &str,
) -> CatalogBundle {
    let payload = serde_json::to_string(kb).unwrap_or_default();
    signed_catalog::sign_bundle(KB_CHANNEL, version, &payload, key, key_id)
}

fn parse_payload(bundle: &CatalogBundle) -> EaiResult<KnowledgeBase> {
    if bundle.channel != KB_CHANNEL {
        return Err(EaiError::config(format!(
            "bundle channel {:?} is not {KB_CHANNEL:?}",
            bundle.channel
        )));
    }
    let kb: KnowledgeBase = serde_json::from_str(&bundle.payload)
        .map_err(|e| EaiError::config(format!("payload is not a knowledge base: {e}")))?;
    let issues = validate(&kb);
    if !issues.is_empty() {
        return Err(EaiError::config(format!(
            "payload fails schema validation ({} issue(s)); first: {} {}",
            issues.len(),
            issues[0].path,
            issues[0].message
        )));
    }
    Ok(kb)
}

fn store_dir(dir: &Path) -> PathBuf {
    dir.join(STORE_SUBDIR)
}

/// Remove `*.json` files in `store/` whose entity id is not in `keep` —
/// entities the update dropped must not linger.
fn prune_stale(store: &Path, kb: &KnowledgeBase) -> EaiResult<()> {
    if !store.is_dir() {
        return Ok(());
    }
    let keep: std::collections::HashSet<&str> = kb.entities.iter().map(|e| e.id()).collect();
    for entry in std::fs::read_dir(store).context("read store dir")? {
        let entry = entry.context("read store entry")?;
        let path = entry.path();
        let is_entity = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| keep.contains(stem));
        if path.extension().is_some_and(|x| x == "json") && !is_entity {
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    Ok(())
}

/// Install a signed KB update: signature check and downgrade/replay
/// rejection (via [`signed_catalog::install`]), payload schema validation,
/// then explode into `store/`. An invalid payload never touches the store;
/// the previous version stays roll-backable.
///
/// # Errors
/// `EaiError::config` on bad signature, downgrade, wrong channel, or an
/// invalid payload; `EaiError::io` on filesystem failure.
pub fn install_kb(
    dir: &Path,
    bundle: &CatalogBundle,
    trusted: &[VerifyingKey],
) -> EaiResult<InstallOutcome> {
    // Validate the payload *before* installing so a signed-but-broken update
    // fails without disturbing the store or the manifest.
    signed_catalog::verify_bundle(bundle, trusted)?;
    let kb = parse_payload(bundle)?;
    let outcome = signed_catalog::install(dir, bundle, trusted)?;
    let store = store_dir(dir);
    crate::eco_store::write_kb(&kb, &store).context("explode knowledge base")?;
    prune_stale(&store, &kb)?;
    Ok(outcome)
}

/// Restore the previous KB version: [`signed_catalog::rollback`] brings the
/// payload/manifest back, then the store is re-exploded from it.
///
/// # Errors
/// `EaiError::config` when no rollback copy exists; `EaiError::io` on
/// filesystem failure.
pub fn rollback_kb(dir: &Path) -> EaiResult<u64> {
    let version = signed_catalog::rollback(dir)?;
    let payload = std::fs::read_to_string(dir.join(signed_catalog::CATALOG_FILE))
        .context("read rolled-back catalog payload")?;
    let kb: KnowledgeBase = serde_json::from_str(&payload)
        .map_err(|e| EaiError::config(format!("rollback payload corrupt: {e}")))?;
    let store = store_dir(dir);
    crate::eco_store::write_kb(&kb, &store).context("re-explode rollback")?;
    prune_stale(&store, &kb)?;
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Component, ComponentCategory, Confidence, Entity, Provenance, Relation, RelationKind,
        Vendor, SCHEMA_VERSION,
    };

    fn prov() -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    fn kb_v(engine_name: &str) -> KnowledgeBase {
        KnowledgeBase {
            version: SCHEMA_VERSION.into(),
            entities: vec![
                Entity::Vendor(Vendor {
                    id: "acme".into(),
                    name: "Acme".into(),
                    home: "https://acme.example.test".into(),
                    provenance: prov(),
                }),
                Entity::Component(Component {
                    id: "engine".into(),
                    name: engine_name.into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov(),
                }),
            ],
            relations: vec![Relation {
                kind: RelationKind::Provides,
                from: "acme".into(),
                to: "engine".into(),
                provenance: prov(),
            }],
        }
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[4u8; 32])
    }

    fn trusted() -> Vec<VerifyingKey> {
        vec![key().verifying_key()]
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("susi-eco-signed-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn eco_signed_kb_installs_and_explodes_to_entity_files() {
        let dir = tmpdir("install");
        let b = bundle_kb(&kb_v("Engine"), 5, &key(), "release");
        let out = install_kb(&dir, &b, &trusted()).unwrap();
        assert_eq!(out.version, 5);
        assert!(dir.join("store/acme.json").is_file());
        assert!(dir.join("store/engine.json").is_file());
        let loaded = crate::eco_store::load_dir(&dir.join("store")).unwrap();
        assert_eq!(loaded.entities.len(), 2);
        assert_eq!(signed_catalog::current_version(&dir), 5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eco_signed_kb_tampered_or_wrong_channel_rejected() {
        let dir = tmpdir("tamper");
        let b = bundle_kb(&kb_v("Engine"), 5, &key(), "release");
        let tampered = CatalogBundle {
            payload: b.payload.replace("Engine", "Evil"),
            ..b.clone()
        };
        assert!(install_kb(&dir, &tampered, &trusted()).is_err());
        let wrong_channel = CatalogBundle {
            channel: "models".into(),
            ..b
        };
        assert!(install_kb(&dir, &wrong_channel, &trusted()).is_err());
        assert!(!dir.join("store").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eco_signed_kb_downgrade_and_replay_rejected() {
        let dir = tmpdir("downgrade");
        install_kb(&dir, &bundle_kb(&kb_v("V2"), 2, &key(), "r"), &trusted()).unwrap();
        assert!(install_kb(&dir, &bundle_kb(&kb_v("V1"), 1, &key(), "r"), &trusted()).is_err());
        assert!(install_kb(&dir, &bundle_kb(&kb_v("V2"), 2, &key(), "r"), &trusted()).is_err());
        let loaded = crate::eco_store::load_dir(&dir.join("store")).unwrap();
        assert_eq!(loaded.entity("engine").unwrap().name(), "V2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eco_signed_kb_rollback_restores_store() {
        let dir = tmpdir("rollback");
        install_kb(&dir, &bundle_kb(&kb_v("V1"), 1, &key(), "r"), &trusted()).unwrap();
        // v2 drops the vendor — rollback must restore it (stale pruning).
        let mut v2 = kb_v("V2");
        v2.entities.retain(|e| e.id() == "engine");
        v2.relations.clear();
        install_kb(&dir, &bundle_kb(&v2, 2, &key(), "r"), &trusted()).unwrap();
        assert!(!dir.join("store/acme.json").exists());

        assert_eq!(rollback_kb(&dir).unwrap(), 1);
        let loaded = crate::eco_store::load_dir(&dir.join("store")).unwrap();
        assert_eq!(loaded.entities.len(), 2);
        assert_eq!(loaded.entity("engine").unwrap().name(), "V1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eco_signed_kb_invalid_payload_never_touches_store() {
        let dir = tmpdir("invalid");
        // Sign an invalid KB (dangling relation) — the channel delivers it,
        // but install must validate and refuse before writing the store.
        let mut bad = kb_v("Bad");
        bad.relations.push(Relation {
            kind: RelationKind::DependsOn,
            from: "engine".into(),
            to: "ghost".into(),
            provenance: prov(),
        });
        let b = bundle_kb(&bad, 1, &key(), "release");
        assert!(install_kb(&dir, &b, &trusted()).is_err());
        assert!(!dir.join("store").exists());
        assert_eq!(signed_catalog::current_version(&dir), 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
