//! Git-friendly on-disk store for the ecosystem knowledge base: one file per
//! entity under `config/ecosystem/` so parallel agents never touch the same
//! file (Mandate 51).
//!
//! Layout:
//!
//! ```text
//! <root>/<entity-id>.json   — {"version", "entity", "relations"}
//! ```
//!
//! A relation is filed inside its *source* entity's file, so each file is a
//! complete set of facts about one entity and merges are trivially
//! conflict-free: two writers editing different entities write different
//! files. Two layers are supported — bundled (ships with susi) and user
//! (host-specific overrides): the user layer wins per entity id, whole file
//! for whole file.
use crate::eco_schema::{validate, Entity, Issue, KnowledgeBase, Relation, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult, ResultExt as Context};

/// Directory name under `SusiDirs::config_dir()` for the user layer.
pub const STORE_DIR: &str = "ecosystem";

/// One entity file: the entity plus its outgoing relations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityFile {
    pub version: String,
    pub entity: Entity,
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// `<id>.json`. Entity ids are `[a-z0-9._:-]` so they are already path-safe;
/// this double-checks before a file name is ever built from untrusted data.
fn file_name_for(id: &str) -> EaiResult<String> {
    if id.is_empty()
        || id.len() > 128
        || id.chars().any(|c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | ':' | '-'))
        })
    {
        return Err(EaiError::config(format!(
            "entity id {id:?} is not a safe file name"
        )));
    }
    Ok(format!("{id}.json"))
}

/// Default user-layer root: `<config>/ecosystem`.
#[must_use]
pub fn default_store_dir() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join(STORE_DIR)
}

/// Bundled-layer source inside this repository (`config/ecosystem/`) —
/// what a release installs as the bundled layer and what offline profile
/// tests validate. `SUSI_ECO_SOURCE` overrides (installed layouts).
#[must_use]
pub fn bundled_source_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SUSI_ECO_SOURCE") {
        return PathBuf::from(d);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/ecosystem")
}

/// Persist a knowledge base as one file per entity. Refuses to write an
/// invalid base — the store is only ever a serialization of facts that
/// already passed [`validate`].
pub fn write_kb(kb: &KnowledgeBase, dir: &Path) -> EaiResult<usize> {
    let issues = validate(kb);
    if !issues.is_empty() {
        return Err(EaiError::config(format!(
            "refusing to store invalid knowledge base ({} issue(s)); first: {} {}",
            issues.len(),
            issues[0].path,
            issues[0].message
        )));
    }
    std::fs::create_dir_all(dir).context("create ecosystem store directory")?;

    // Group outgoing relations by source entity.
    let mut outgoing: std::collections::HashMap<&str, Vec<Relation>> =
        std::collections::HashMap::new();
    for r in &kb.relations {
        outgoing.entry(r.from.as_str()).or_default().push(r.clone());
    }

    let mut written = 0usize;
    for entity in &kb.entities {
        let file = EntityFile {
            version: kb.version.clone(),
            entity: entity.clone(),
            relations: outgoing.get(entity.id()).cloned().unwrap_or_default(),
        };
        let path = dir.join(file_name_for(entity.id())?);
        susi_config::atomic_write_json_pretty(&path, &file)
            .with_context(|| format!("write {}", path.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Load every `<id>.json` under `dir` into a knowledge base. Missing or
/// unreadable directories yield an empty base; malformed files are an error —
/// a corrupt fact must surface, not silently drop.
pub fn load_dir(dir: &Path) -> EaiResult<KnowledgeBase> {
    let mut kb = KnowledgeBase::new();
    if !dir.is_dir() {
        return Ok(kb);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .context("read ecosystem store directory")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    for path in paths {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let file: EntityFile =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if file.version != SCHEMA_VERSION {
            return Err(EaiError::config(format!(
                "{}: schema version {:?}, expected {SCHEMA_VERSION:?}",
                path.display(),
                file.version
            )));
        }
        kb.entities.push(file.entity);
        kb.relations.extend(file.relations);
    }
    Ok(kb)
}

/// Merge a bundled layer with a user layer. The user layer replaces bundled
/// entities with the same id — and with them that entity's outgoing
/// relations, since relations are filed with their source. Additions on
/// either side union.
pub fn load(bundled: &Path, user: &Path) -> EaiResult<KnowledgeBase> {
    let base = load_dir(bundled)?;
    let over = load_dir(user)?;

    // The user layer replaces a bundled entity whole-file: the overridden
    // entity's bundled outgoing relations go with it, so relations are
    // filtered against the ids that survive *from the same layer*.
    let overridden: std::collections::HashSet<&str> =
        over.entities.iter().map(|e| e.id()).collect();
    let user_ids: std::collections::HashSet<&str> = overridden.clone();
    let mut entities: Vec<Entity> = base
        .entities
        .iter()
        .filter(|e| !overridden.contains(e.id()))
        .cloned()
        .collect();
    entities.extend(over.entities.iter().cloned());
    let mut relations: Vec<Relation> = base
        .relations
        .iter()
        .filter(|r| {
            !overridden.contains(r.from.as_str()) && entities.iter().any(|e| e.id() == r.from)
        })
        .cloned()
        .collect();
    relations.extend(
        over.relations
            .iter()
            .filter(|r| user_ids.contains(r.from.as_str()))
            .cloned(),
    );
    let mut kb = KnowledgeBase::new();
    kb.entities = entities;
    kb.relations = relations;
    Ok(kb)
}

/// Validate a store directory as a knowledge base — the check CI runs over
/// the bundled layer and `susi ecosystem kb` runs over the merged one.
#[must_use]
pub fn validate_store(dir: &Path) -> Vec<Issue> {
    match load_dir(dir) {
        Ok(kb) => validate(&kb),
        Err(e) => vec![Issue {
            stage: crate::eco_schema::Stage::Model,
            path: String::new(),
            message: format!("store does not load: {e}"),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Capability, CapabilityClass, Component, ComponentCategory, Confidence, Provenance,
        RelationKind, Vendor,
    };

    fn prov() -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    fn kb() -> KnowledgeBase {
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
                    id: "acme-engine".into(),
                    name: "Engine".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov(),
                }),
                Entity::Capability(Capability {
                    id: "cap-tools".into(),
                    name: "Tools".into(),
                    class: CapabilityClass::ToolCalling,
                    provenance: prov(),
                }),
            ],
            relations: vec![
                Relation {
                    kind: RelationKind::Provides,
                    from: "acme".into(),
                    to: "acme-engine".into(),
                    provenance: prov(),
                },
                Relation {
                    kind: RelationKind::HasCapability,
                    from: "acme-engine".into(),
                    to: "cap-tools".into(),
                    provenance: prov(),
                },
            ],
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-eco-store-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_then_load_roundtrips() {
        let dir = tmpdir("roundtrip");
        let kb = kb();
        let n = write_kb(&kb, &dir).unwrap();
        assert_eq!(n, 3);
        assert!(dir.join("acme.json").is_file());
        assert!(dir.join("acme-engine.json").is_file());
        let loaded = load_dir(&dir).unwrap();
        assert_eq!(loaded.entities.len(), 3);
        assert_eq!(loaded.relations.len(), 2);
        // Relations travel inside their source entity's file.
        let engine_file: EntityFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join("acme-engine.json")).unwrap())
                .unwrap();
        assert_eq!(engine_file.relations.len(), 1);
        assert_eq!(engine_file.relations[0].kind, RelationKind::HasCapability);
        assert!(validate(&loaded).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn relations_are_filed_with_their_source_entity() {
        let dir = tmpdir("ownership");
        write_kb(&kb(), &dir).unwrap();
        let acme: EntityFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join("acme.json")).unwrap()).unwrap();
        assert_eq!(acme.relations.len(), 1);
        assert_eq!(acme.relations[0].kind, RelationKind::Provides);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn user_layer_overrides_whole_entity_files() {
        let bundled = tmpdir("bundled");
        let user = tmpdir("user");
        write_kb(&kb(), &bundled).unwrap();

        // User layer replaces acme-engine: renamed, and its outgoing
        // has-capability edge replaced by nothing (whole-file override).
        let mut override_file: EntityFile = serde_json::from_str(
            &std::fs::read_to_string(bundled.join("acme-engine.json")).unwrap(),
        )
        .unwrap();
        if let Entity::Component(c) = &mut override_file.entity {
            c.name = "Engine (user)".into();
        }
        override_file.relations.clear();
        susi_config::atomic_write_json_pretty(&user.join("acme-engine.json"), &override_file)
            .unwrap();

        let merged = load(&bundled, &user).unwrap();
        assert_eq!(merged.entities.len(), 3);
        let engine = merged.entity("acme-engine").unwrap();
        assert_eq!(engine.name(), "Engine (user)");
        // acme's provides survives; the overridden engine's edge is gone.
        assert_eq!(merged.relations.len(), 1);
        assert_eq!(merged.relations[0].kind, RelationKind::Provides);
        std::fs::remove_dir_all(&bundled).ok();
        std::fs::remove_dir_all(&user).ok();
    }

    #[test]
    fn user_layer_adds_entities() {
        let bundled = tmpdir("add-bundled");
        let user = tmpdir("add-user");
        write_kb(&kb(), &bundled).unwrap();
        let extra = EntityFile {
            version: SCHEMA_VERSION.into(),
            entity: Entity::Capability(Capability {
                id: "cap-extra".into(),
                name: "Extra".into(),
                class: CapabilityClass::Streaming,
                provenance: prov(),
            }),
            relations: vec![],
        };
        susi_config::atomic_write_json_pretty(&user.join("cap-extra.json"), &extra).unwrap();
        let merged = load(&bundled, &user).unwrap();
        assert!(merged.entity("cap-extra").is_some());
        assert_eq!(merged.entities.len(), 4);
        std::fs::remove_dir_all(&bundled).ok();
        std::fs::remove_dir_all(&user).ok();
    }

    #[test]
    fn write_refuses_an_invalid_kb() {
        let dir = tmpdir("invalid");
        let mut kb = kb();
        kb.relations.push(Relation {
            kind: RelationKind::HasCapability,
            from: "acme-engine".into(),
            to: "ghost".into(),
            provenance: prov(),
        });
        assert!(write_kb(&kb, &dir).is_err());
        assert!(!dir.join("acme.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_is_parse_strict_and_version_checked() {
        let dir = tmpdir("corrupt");
        std::fs::write(dir.join("bad.json"), "{not json").unwrap();
        assert!(load_dir(&dir).is_err());

        let dir2 = tmpdir("versioned");
        std::fs::write(
            dir2.join("x.json"),
            r#"{"version":"eco-schema/v0","entity":{"kind":"capability","id":"c","name":"c","class":"audio","provenance":{"source":"https://a.b/c","spec_version":"1","retrieved":"2026-01-01","confidence":"verified"}}}"#,
        )
        .unwrap();
        assert!(load_dir(&dir2).unwrap_err().to_string().contains("v0"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn missing_dirs_load_empty_and_validate_clean() {
        let dir = tmpdir("missing").join("nope");
        let kb = load_dir(&dir).unwrap();
        assert!(kb.entities.is_empty() && kb.relations.is_empty());
        assert!(validate_store(&dir).is_empty());
    }

    #[test]
    fn file_names_reject_path_traversal() {
        assert!(file_name_for("ok-id.1").is_ok());
        assert!(file_name_for("../escape").is_err());
        assert!(file_name_for("a/b").is_err());
        assert!(file_name_for("").is_err());
    }
}
