//! Open-core ecosystem query contract (`susi ecosystem kb` and `gemi.ecosystem.*` topics).
//! Full commercial KB graph/schema/consistency lives in `susi-commercial-apps` (`susi-ecosystem-hub`).

use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Default, Clone, Copy)]
pub struct Filters<'a> {
    pub query: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub capability: Option<&'a str>,
    pub vendor: Option<&'a str>,
}

fn core_entities() -> Vec<Value> {
    vec![
        json!({"id": "mcp", "kind": "protocol", "name": "Model Context Protocol"}),
        json!({"id": "a2a", "kind": "protocol", "name": "Agent2Agent Protocol"}),
        json!({"id": "openai-chat-completions", "kind": "protocol", "name": "OpenAI Chat Completions"}),
    ]
}

pub fn list(kind: Option<&str>, _dir: Option<&Path>) -> Result<Value, String> {
    let items: Vec<Value> = core_entities()
        .into_iter()
        .filter(|e| kind.is_none_or(|k| e["kind"].as_str() == Some(k)))
        .collect();
    Ok(Value::Array(items))
}

pub fn show(id: &str, _dir: Option<&Path>) -> Result<Value, String> {
    let Some(entity) = core_entities()
        .into_iter()
        .find(|e| e["id"].as_str() == Some(id))
    else {
        return Err(format!(
            "no ecosystem entity {id:?} — try `susi ecosystem kb search`"
        ));
    };
    Ok(json!({
        "entity": entity,
        "outgoing": [],
        "incoming": [],
    }))
}

pub fn search(filters: &Filters, _dir: Option<&Path>) -> Result<Value, String> {
    let found: Vec<Value> = core_entities()
        .into_iter()
        .filter(|e| {
            filters.kind.is_none_or(|k| e["kind"].as_str() == Some(k))
                && filters.query.is_none_or(|q| {
                    let ql = q.to_lowercase();
                    e["id"].as_str().unwrap_or_default().contains(&ql)
                        || e["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_lowercase()
                            .contains(&ql)
                })
        })
        .collect();
    Ok(json!({ "count": found.len(), "entities": found }))
}

pub fn check(_dir: Option<&Path>) -> Result<Value, String> {
    Ok(json!({
        "entities": 3,
        "relations": 0,
        "issues": [],
    }))
}

pub fn explain(id: &str, dir: Option<&Path>) -> Result<Value, String> {
    if dir.is_some_and(|d| !d.is_absolute()) {
        return Err("dir must be an absolute path".to_string());
    }
    let shown = show(id, dir)?;
    Ok(json!({
        "entity": shown["entity"],
        "versions": [],
        "implemented_by": [],
        "provided_by": [],
        "capabilities": [],
        "susi_profile": Value::Null,
    }))
}

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
