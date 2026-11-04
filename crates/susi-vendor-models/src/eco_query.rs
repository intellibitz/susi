//! Queries over the cloud-AI-ecosystem knowledge base: the documents
//! `susi ecosystem kb list|show|search|check` and `susi ecosystem explain`
//! print, and the MCP `ecosystem_kb` / `ecosystem_explain` tools return
//! (zero-UI plan B4 group 4). One implementation for the CLI and the socket.

use crate::eco_schema::{Entity, EntityKind, KnowledgeBase, RelationKind};
use crate::{eco_consistency, eco_profile, eco_relations, eco_store, eco_taxonomy};
use serde_json::{json, Value};
use std::path::Path;

/// Load the layered bundled+user store, or one store dir when `dir` is set.
pub fn load_kb(dir: Option<&Path>) -> Result<KnowledgeBase, String> {
    match dir {
        Some(d) => eco_store::load_dir(d).map_err(|e| format!("load {}: {e}", d.display())),
        None => eco_store::load_default().map_err(|e| format!("load layered ecosystem store: {e}")),
    }
}

/// Entity filters shared by `kb list` and `kb search`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Filters<'a> {
    /// Case-insensitive substring matched against id and name.
    pub query: Option<&'a str>,
    /// Restrict to one kind label.
    pub kind: Option<&'a str>,
    /// Keep components having this capability (any vendor spelling).
    pub capability: Option<&'a str>,
    /// Keep the vendor itself and components it `provides`.
    pub vendor: Option<&'a str>,
}

fn matches(kb: &KnowledgeBase, e: &Entity, f: &Filters) -> bool {
    if let Some(k) = f.kind {
        if e.kind().label() != k {
            return false;
        }
    }
    if let Some(q) = f.query {
        let q = q.to_lowercase();
        if !(e.id().contains(q.as_str()) || e.name().to_lowercase().contains(q.as_str())) {
            return false;
        }
    }
    if let Some(v) = f.vendor {
        let g = eco_relations::Graph::new(kb);
        let ok = (e.kind() == EntityKind::Vendor && e.id() == v)
            || g.providers(e.id()).iter().any(|p| p.id() == v);
        if !ok {
            return false;
        }
    }
    if let Some(c) = f.capability {
        let g = eco_relations::Graph::new(kb);
        let canonical = eco_taxonomy::canonical(c).map(|c| c.id);
        let ok = g.capabilities_of(e.id()).iter().any(|cap| {
            Some(cap.id()) == canonical || cap.id() == c || cap.name().eq_ignore_ascii_case(c)
        });
        if !ok {
            return false;
        }
    }
    true
}

fn entity_json(e: &Entity) -> Value {
    json!({
        "id": e.id(),
        "kind": e.kind().label(),
        "name": e.name(),
    })
}

fn rows(kb: &KnowledgeBase, f: &Filters) -> Vec<Value> {
    kb.entities
        .iter()
        .filter(|e| matches(kb, e, f))
        .map(entity_json)
        .collect()
}

/// `susi ecosystem kb list [--kind]`: entity ids, kinds and names.
pub fn list(kind: Option<&str>, dir: Option<&Path>) -> Result<Value, String> {
    let kb = load_kb(dir)?;
    Ok(Value::Array(rows(
        &kb,
        &Filters {
            kind,
            ..Filters::default()
        },
    )))
}

/// `susi ecosystem kb show <id>`: one entity with its incoming and
/// outgoing relations.
pub fn show(id: &str, dir: Option<&Path>) -> Result<Value, String> {
    let kb = load_kb(dir)?;
    let Some(e) = kb.entity(id) else {
        return Err(format!(
            "no ecosystem entity {id:?} — try `susi ecosystem kb search`"
        ));
    };
    let outgoing: Vec<_> = kb.relations.iter().filter(|r| r.from == id).collect();
    let incoming: Vec<_> = kb.relations.iter().filter(|r| r.to == id).collect();
    Ok(json!({
        "entity": e,
        "outgoing": outgoing,
        "incoming": incoming,
    }))
}

/// `susi ecosystem kb search`: entities passing every filter.
pub fn search(filters: &Filters, dir: Option<&Path>) -> Result<Value, String> {
    let kb = load_kb(dir)?;
    let found = rows(&kb, filters);
    Ok(json!({ "count": found.len(), "entities": found }))
}

/// `susi ecosystem kb check`: schema + consistency issues over the store.
pub fn check(dir: Option<&Path>) -> Result<Value, String> {
    let kb = load_kb(dir)?;
    let issues = eco_consistency::check(&kb);
    Ok(json!({
        "entities": kb.entities.len(),
        "relations": kb.relations.len(),
        "issues": issues,
    }))
}

/// `susi ecosystem explain <id>`: one entity explained end to end — what it
/// is, every recorded version, who publishes/implements it, its
/// capabilities, the profile susi uses for it and the provenance cited.
pub fn explain(id: &str, dir: Option<&Path>) -> Result<Value, String> {
    let kb = load_kb(dir)?;
    let Some(e) = kb.entity(id) else {
        return Err(format!(
            "no ecosystem entity {id:?} — try `susi ecosystem kb search`"
        ));
    };
    let g = eco_relations::Graph::new(&kb);
    let versions: Vec<String> = kb
        .relations
        .iter()
        .filter(|r| r.to == id && r.kind == RelationKind::VersionOf)
        .map(|r| r.from.clone())
        .collect();
    // implementers hit the entity directly (`implements`) or through one of
    // its versions (`implements-version` a spec-version that is version-of it)
    let mut implementers: Vec<String> = kb
        .relations
        .iter()
        .filter(|r| r.to == id && r.kind == RelationKind::Implements)
        .map(|r| r.from.clone())
        .collect();
    for v in &versions {
        implementers.extend(
            kb.relations
                .iter()
                .filter(|r| r.to == *v && r.kind == RelationKind::ImplementsVersion)
                .map(|r| r.from.clone()),
        );
    }
    implementers.sort();
    implementers.dedup();
    let providers: Vec<String> = g.providers(id).iter().map(|p| p.id().to_string()).collect();
    let capabilities: Vec<String> = g
        .capabilities_of(id)
        .iter()
        .map(|c| c.id().to_string())
        .collect();
    let profile = eco_profile::for_subject(id, dir).map_err(|e| e.to_string())?;
    Ok(json!({
        "entity": e,
        "versions": versions,
        "implemented_by": implementers,
        "provided_by": providers,
        "capabilities": capabilities,
        "susi_profile": profile,
    }))
}

/// Dispatch one `kb` query: `{action: list|show|search|check, id, query,
/// kind, capability, vendor, dir}` (`dir` absolute). Absent `action` lists.
pub fn kb_query(payload: &Value) -> Result<Value, String> {
    let field = |key: &str| payload.get(key).and_then(|v| v.as_str());
    let dir = field("dir").map(Path::new);
    if let Some(d) = dir {
        if !d.is_absolute() {
            return Err("dir must be an absolute path".to_string());
        }
    }
    match field("action").unwrap_or("list") {
        "list" => list(field("kind"), dir),
        "show" => show(field("id").ok_or("show requires id")?, dir),
        "search" => search(
            &Filters {
                query: field("query"),
                kind: field("kind"),
                capability: field("capability"),
                vendor: field("vendor"),
            },
            dir,
        ),
        "check" => check(dir),
        other => Err(format!(
            "unknown kb action `{other}` (expected list, show, search or check)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_store_answers_every_query() {
        let listed = kb_query(&json!({})).unwrap();
        let first = listed.as_array().unwrap().first().cloned().unwrap();
        let id = first["id"].as_str().unwrap();
        let shown = kb_query(&json!({"action": "show", "id": id})).unwrap();
        assert_eq!(shown["entity"]["id"], id);
        let found = kb_query(&json!({"action": "search", "query": id})).unwrap();
        assert!(found["count"].as_u64().unwrap() >= 1);
        let checked = kb_query(&json!({"action": "check"})).unwrap();
        assert!(checked["entities"].as_u64().unwrap() > 0);
        assert_eq!(explain(id, None).unwrap()["entity"]["id"], id);
    }

    #[test]
    fn bad_queries_are_refused() {
        assert!(kb_query(&json!({"action": "show"})).is_err());
        assert!(kb_query(&json!({"action": "nope"})).is_err());
        assert!(kb_query(&json!({"dir": "relative"}))
            .unwrap_err()
            .contains("absolute"));
        assert!(explain("definitely-not-an-entity", None).is_err());
    }
}
