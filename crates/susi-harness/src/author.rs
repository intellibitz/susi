//! Roadmap authoring.
//!
//! The supervisor's first job is to turn a roadmap vector into a plan. That is
//! a model call, so it goes through the metering gateway like every other call:
//! the prompt is fixed, the answer must be a validated plan, and the spend is
//! a ledger row tagged `authoring/<vector>`.

use crate::supervisor::Plan;
use serde_json::{json, Value};
use susi_error::{EaiError, EaiResult};

/// The fixed instruction every authoring call carries.
pub const AUTHOR_SYSTEM: &str = "You are SUSI's build supervisor. Return only a JSON object with `mission` (a short id) and `tasks` (array). Each task has: `key` (kebab-case, unique), `title`, `accept` (an argv string), `scopes` (repo-relative paths), `model` (one of the listed workers), and `prompt` (a self-contained instruction for a coding agent). No prose outside the JSON. Hard rules: at most the requested number of tasks; `accept` must be test-shaped (`cargo test ...` or `cargo nextest run ...`) or an existing `scripts/` checker; scopes are paths that exist or are clearly new; do not include work the narrative says is already delivered.";

/// Where the authoring call goes.
pub struct AuthorConfig {
    /// Gateway base URL.
    pub gateway_url: String,
    /// Model id.
    pub model: String,
    /// Call timeout, in seconds.
    pub timeout_secs: u64,
}

/// What to author from.
pub struct AuthorRequest {
    /// Vector id and title, e.g. `VC-202-025 agent identity`.
    pub vector: String,
    /// The vector's mastery target.
    pub target: String,
    /// Workspace package names, so acceptance can use exact `-p` ids.
    pub packages: Vec<String>,
    /// Worker models a task may be assigned: each task gets exactly one.
    pub workers: Vec<String>,
    /// Recent narrative (may be truncated).
    pub narrative: String,
    /// Upper bound on tasks.
    pub max_tasks: usize,
}

/// The user prompt for one authoring call.
#[must_use]
pub fn prompt(request: &AuthorRequest) -> String {
    format!(
        "Roadmap vector: {vector}\nMastery target: {target}\nWorkspace packages (use these exact names in `-p`): {packages}\nWorker models (assign each task exactly one as `model`): {workers}\nMost recent narrative (may be truncated):\n{narrative}\n\nReturn a plan of at most {max_tasks} tasks. Prefer one small, self-contained increment that one agent can finish in under 15 minutes; if the vector is large, plan only its first concrete step. Each task must be executable by one coding agent in one worktree, and its scopes must not overlap another task's. Spread the tasks across the workers when more than one is listed.",
        vector = request.vector,
        target = request.target,
        packages = request.packages.join(", "),
        workers = request.workers.join(", "),
        narrative = request.narrative,
        max_tasks = request.max_tasks
    )
}

/// Ask the gateway's model for a plan and validate it.
///
/// # Errors
///
/// Returns `EaiError::network` when the call fails or the response carries no
/// message, and `EaiError::config` when the answer is not a valid plan.
pub fn author_plan(config: &AuthorConfig, request: &AuthorRequest) -> EaiResult<Plan> {
    let body = json!({
        "model": config.model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": AUTHOR_SYSTEM },
            { "role": "user", "content": prompt(request) }
        ]
    });
    let body = serde_json::to_string(&body)
        .map_err(|error| EaiError::network(format!("encode authoring request: {error}")))?;
    let url = format!(
        "{}/v1/chat/completions",
        config.gateway_url.trim_end_matches('/')
    );
    let authorization = format!("Bearer susi:authoring:{}:1", request.vector);
    let call = susi_http_transport::http_call_with_body(
        "POST",
        &url,
        &[
            ("authorization", authorization.as_str()),
            ("content-type", "application/json"),
        ],
        Some(body.as_bytes()),
        config.timeout_secs,
        0,
    )
    .map_err(|error| EaiError::network(format!("authoring call: {error}")))?;
    let status = call.status;
    let text = call
        .into_utf8(2 * 1024 * 1024)
        .map_err(|error| EaiError::network(format!("authoring body: {error}")))?;
    if !(200..300).contains(&status) {
        return Err(EaiError::network(format!(
            "authoring gateway returned {status}: {}",
            tail(&text)
        )));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| EaiError::network(format!("authoring response: {error}")))?;
    let content = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| EaiError::network("authoring response has no message content"))?;
    parse_plan(content)
}

/// Parse and validate a plan from a model answer, tolerating code fences.
///
/// # Errors
///
/// Returns `EaiError::config` when the answer is not a valid plan.
pub fn parse_plan(content: &str) -> EaiResult<Plan> {
    let text = strip_fences(content);
    let plan: Plan = serde_json::from_str(text)
        .map_err(|error| EaiError::config(format!("authored plan is not JSON: {error}")))?;
    plan.validate()?;
    Ok(plan)
}

fn strip_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim();
    }
    trimmed
}

fn tail(text: &str) -> String {
    const LIMIT: usize = 800;
    if text.len() <= LIMIT {
        return text.to_string();
    }
    text.get(text.len() - LIMIT..).unwrap_or(text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AuthorRequest {
        AuthorRequest {
            vector: "VC-202-025 agent identity".into(),
            target: "durable worker identity".into(),
            packages: vec!["susi-harness".into(), "susi-gmcp".into()],
            workers: vec!["deepseek-flash".into(), "gemini-flash".into()],
            narrative: "recent work".into(),
            max_tasks: 3,
        }
    }

    #[test]
    fn the_prompt_carries_the_vector_the_packages_the_workers_and_the_limit() {
        let text = prompt(&request());
        assert!(text.contains("VC-202-025"));
        assert!(text.contains("durable worker identity"));
        assert!(text.contains("susi-harness"));
        assert!(text.contains("susi-gmcp"));
        assert!(text.contains("deepseek-flash"));
        assert!(text.contains("gemini-flash"));
        assert!(text.contains("at most 3 tasks"));
        assert!(text.contains("under 15 minutes"));
    }

    #[test]
    fn a_task_can_name_its_worker_model() {
        let content = r#"{
          "mission": "m",
          "tasks": [{
            "key": "a",
            "title": "t",
            "accept": "cargo test -p susi-harness",
            "scopes": ["docs/harness.md"],
            "model": "gemini-flash",
            "prompt": "p"
          }]
        }"#;
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.tasks[0].model.as_deref(), Some("gemini-flash"));
    }

    #[test]
    fn a_fenced_plan_parses_and_validates() {
        let content = r#"```json
        {
          "mission": "vc-202-025",
          "tasks": [{
            "key": "docs",
            "title": "document the identity",
            "accept": "cargo test -p susi-harness",
            "scopes": ["docs/harness.md"],
            "prompt": "write the docs"
          }]
        }
        ```"#;
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].key, "docs");
    }

    #[test]
    fn an_invalid_plan_is_refused() {
        assert!(parse_plan(r#"{"mission":"m","tasks":[]}"#).is_err());
        assert!(parse_plan("not json").is_err());
    }
}
