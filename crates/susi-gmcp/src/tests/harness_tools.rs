use super::*;
use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use std::sync::Arc;

struct Echo;
impl PlaneHandler for Echo {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        Ok(json!({ "payload": payload }))
    }
}

fn sent(result: EaiResult<String>) -> Value {
    serde_json::from_str::<Value>(&result.unwrap()).unwrap()["payload"].clone()
}

#[test]
fn harness_tools_require_and_confine_the_repository() {
    PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let ledger = ws.join("susi-harness-tool-l.jsonl");
    let status = sent(CoreTools::harness_status(
        &json!({"ledger": ledger.display().to_string()}),
        &ws,
    ));
    assert_eq!(status["action"], "status");
    assert!(Path::new(status["ledger"].as_str().unwrap()).ends_with("susi-harness-tool-l.jsonl"));
    // Every caller-named file path is absolute and policy-confined.
    assert!(
        CoreTools::harness_status(&json!({"ledger": "rel.jsonl"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    assert_eq!(
        sent(CoreTools::harness_budget(&json!({}), &ws)),
        json!({"action": "budget"})
    );
    assert!(CoreTools::harness_plan(&json!({"out": "/p.json"}), &ws)
        .unwrap_err()
        .to_string()
        .contains("repo is required"));
    assert!(
        CoreTools::harness_run(&json!({"repo": "relative", "plan": "/p"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    let repo_dir = std::env::temp_dir().join(format!("susi-harness-tool-{}", std::process::id()));
    std::fs::create_dir_all(&repo_dir).unwrap();
    let repo_text = repo_dir.display().to_string();
    let out = repo_dir.join("plan.json").display().to_string();
    let planned = sent(CoreTools::harness_plan(
        &json!({"repo": repo_text, "out": out, "limit": 2}),
        &ws,
    ));
    assert_eq!(planned["limit"], 2);
    assert!(Path::new(planned["out"].as_str().unwrap()).ends_with("plan.json"));
    assert!(
        CoreTools::harness_plan(&json!({"repo": repo_text, "out": "p.json"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    // The binary a run drives is never the caller's choice.
    let chosen = CoreTools::harness_run(
        &json!({"repo": repo_text, "plan": out, "gateway": "g", "susi": "/bin/sh"}),
        &ws,
    )
    .unwrap_err();
    assert!(
        chosen.to_string().contains("susi cannot be chosen"),
        "{chosen}"
    );
    let run = sent(CoreTools::harness_run(
        &json!({"repo": repo_text, "plan": out, "gateway": "g"}),
        &ws,
    ));
    assert!(run.get("susi").is_none());
    assert!(Path::new(run["plan"].as_str().unwrap()).ends_with("plan.json"));
    assert_eq!(
        Path::new(planned["repo"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        repo_dir.canonicalize().unwrap()
    );
    assert!(CoreTools::harness_models(&json!({"mode": 1}), &ws).is_err());
    assert_eq!(
        sent(CoreTools::harness_runs(&json!({}), &ws)),
        json!({"action": "runs"})
    );
    let _ = std::fs::remove_dir_all(&repo_dir);
}

#[test]
fn harness_models_forwards_mode_provider_and_optional_repo() {
    PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let out = sent(CoreTools::harness_models(
        &json!({"mode": "available", "provider": "openrouter"}),
        &ws,
    ));
    assert_eq!(out["action"], "models");
    assert_eq!(out["mode"], "available");
    assert_eq!(out["provider"], "openrouter");
}

#[test]
fn harness_author_requires_repo_and_confines_out_and_roadmap_paths() {
    PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    assert!(CoreTools::harness_author(&json!({"vector": "VC-1"}), &ws)
        .unwrap_err()
        .to_string()
        .contains("repo is required"));

    let repo_dir = std::env::temp_dir().join(format!("susi-harness-author-{}", std::process::id()));
    std::fs::create_dir_all(&repo_dir).unwrap();
    let out_file = repo_dir.join("authored.json");
    let authored = sent(CoreTools::harness_author(
        &json!({
            "repo": repo_dir.display().to_string(),
            "gateway": "http://127.0.0.1:9000",
            "vector": "VC-201-001",
            "out": out_file.display().to_string(),
            "max_tasks": 4
        }),
        &ws,
    ));
    assert_eq!(authored["action"], "author");
    assert_eq!(authored["vector"], "VC-201-001");
    assert_eq!(authored["max_tasks"], 4);
    let _ = std::fs::remove_dir_all(&repo_dir);
}

#[test]
fn harness_budget_confines_optional_ledger_and_repo_arguments() {
    PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let ledger = ws.join("budget-ledger.jsonl");
    let budget = sent(CoreTools::harness_budget(
        &json!({"ledger": ledger.display().to_string()}),
        &ws,
    ));
    assert_eq!(budget["action"], "budget");
    assert!(Path::new(budget["ledger"].as_str().unwrap()).ends_with("budget-ledger.jsonl"));
    assert!(CoreTools::harness_budget(&json!({"ledger": "relative.jsonl"}), &ws).is_err());
}

#[test]
fn harness_status_and_run_reject_relative_events_or_plan_paths() {
    PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    assert!(CoreTools::harness_status(&json!({"events": "rel-events.jsonl"}), &ws).is_err());
    assert!(CoreTools::harness_run(
        &json!({
            "repo": ws.display().to_string(),
            "plan": "rel-plan.json",
            "gateway": "http://127.0.0.1:9000"
        }),
        &ws
    )
    .is_err());
}
