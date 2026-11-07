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

impl AuthorConfig {
    /// Verify structural invariants of an [`AuthorConfig`].
    ///
    /// # Errors
    ///
    /// Returns a human-readable error if `gateway_url` does not start with
    /// `http://` or `https://`, `model` is blank, or `timeout_secs` is zero.
    pub fn verify_config_invariants(&self) -> Result<(), String> {
        let url = self.gateway_url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("AuthorConfig.gateway_url must start with http:// or https://".to_string());
        }
        if self.model.trim().is_empty() {
            return Err("AuthorConfig.model must not be empty".to_string());
        }
        if self.timeout_secs == 0 {
            return Err("AuthorConfig.timeout_secs must be greater than zero".to_string());
        }
        Ok(())
    }
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
    /// The previous answer's validation error, when this call is a retry.
    pub retry_hint: Option<String>,
}

impl AuthorRequest {
    /// Verify structural invariants of an [`AuthorRequest`].
    ///
    /// # Errors
    ///
    /// Returns a human-readable error if `vector` or `target` is blank,
    /// `packages` or `workers` is empty or contains blank entries, `max_tasks`
    /// is zero, or `retry_hint` is `Some` with a blank string.
    pub fn verify_request_invariants(&self) -> Result<(), String> {
        if self.vector.trim().is_empty() {
            return Err("AuthorRequest.vector must not be empty".to_string());
        }
        if self.target.trim().is_empty() {
            return Err("AuthorRequest.target must not be empty".to_string());
        }
        if self.packages.is_empty() {
            return Err("AuthorRequest.packages must not be empty".to_string());
        }
        if self.packages.iter().any(|pkg| pkg.trim().is_empty()) {
            return Err("AuthorRequest.packages entries must not be empty".to_string());
        }
        if self.workers.is_empty() {
            return Err("AuthorRequest.workers must not be empty".to_string());
        }
        if self.workers.iter().any(|worker| worker.trim().is_empty()) {
            return Err("AuthorRequest.workers entries must not be empty".to_string());
        }
        if self.max_tasks == 0 {
            return Err("AuthorRequest.max_tasks must be greater than zero".to_string());
        }
        if let Some(ref hint) = self.retry_hint {
            if hint.trim().is_empty() {
                return Err("AuthorRequest.retry_hint must not be blank when present".to_string());
            }
        }
        Ok(())
    }
}

/// The user prompt for one authoring call.
#[must_use]
pub fn prompt(request: &AuthorRequest) -> String {
    let mut text = format!(
        "Roadmap vector: {vector}\nMastery target: {target}\nWorkspace packages (use these exact names in `-p`): {packages}\nWorker models (assign each task exactly one as `model`): {workers}\nMost recent narrative (may be truncated):\n{narrative}\n\nReturn a plan of at most {max_tasks} tasks. Prefer one small, self-contained increment that one agent can finish in under 15 minutes; if the vector is large, plan only its first concrete step. Each task must be executable by one coding agent in one worktree, and its scopes must not overlap another task's. Spread the tasks across the workers when more than one is listed.",
        vector = request.vector,
        target = request.target,
        packages = request.packages.join(", "),
        workers = request.workers.join(", "),
        narrative = request.narrative,
        max_tasks = request.max_tasks
    );
    if let Some(hint) = &request.retry_hint {
        text.push_str(&format!(
            "\n\nThe previous answer was rejected: {hint}\nReturn a corrected JSON object that fixes exactly this."
        ));
    }
    text
}

/// Ask the gateway's model for a plan and validate it.
///
/// # Errors
///
/// Returns `EaiError::network` when the call fails or the response carries no
/// message, and `EaiError::config` when the answer is not a valid plan.
pub fn author_plan(config: &AuthorConfig, request: &AuthorRequest) -> EaiResult<Plan> {
    let attempt = if request.retry_hint.is_some() { 2 } else { 1 };
    let phase = if request.retry_hint.is_some() {
        crate::smart::TurnPhase::ErrorRecovery
    } else {
        crate::smart::TurnPhase::InitialPlanning
    };
    let smart_params = crate::smart::SmartParams::for_turn(
        &config.model,
        "authoring",
        &request.vector,
        attempt,
        phase,
    );
    let mut body = json!({
        "model": config.model,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": AUTHOR_SYSTEM },
            { "role": "user", "content": prompt(request) }
        ]
    });
    smart_params.apply_to_request(&mut body);
    let body = serde_json::to_string(&body)
        .map_err(|error| EaiError::network(format!("encode authoring request: {error}")))?;
    let url = format!(
        "{}/v1/chat/completions",
        config.gateway_url.trim_end_matches('/')
    );
    let authorization = format!("Bearer susi:authoring:{}:{attempt}", request.vector);
    let call = susi_http_transport::http_call_with_body(
        "POST",
        &url,
        &[
            ("authorization", authorization.as_str()),
            ("content-type", "application/json"),
            ("x-susi-cache-scope", smart_params.cache_scope.as_str()),
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
            retry_hint: None,
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
    #[test]
    fn a_retry_prompt_carries_the_validation_error() {
        let mut request = request();
        request.retry_hint = Some("scopes must exist under the repository".into());
        let text = prompt(&request);
        assert!(text.contains("previous answer was rejected"));
        assert!(text.contains("scopes must exist under the repository"));
    }

    #[test]
    fn author_config_and_request_verify_invariants_reject_corrupt_state() {
        let valid_cfg = AuthorConfig {
            gateway_url: "https://gateway.local:9185".to_string(),
            model: "deepseek-flash".to_string(),
            timeout_secs: 30,
        };
        assert!(valid_cfg.verify_config_invariants().is_ok());

        let bad_url = AuthorConfig {
            gateway_url: "ftp://gateway.local".to_string(),
            model: valid_cfg.model.clone(),
            timeout_secs: valid_cfg.timeout_secs,
        };
        assert!(bad_url.verify_config_invariants().is_err());

        let blank_model = AuthorConfig {
            gateway_url: valid_cfg.gateway_url.clone(),
            model: "   ".to_string(),
            timeout_secs: valid_cfg.timeout_secs,
        };
        assert!(blank_model.verify_config_invariants().is_err());

        let zero_timeout = AuthorConfig {
            gateway_url: valid_cfg.gateway_url,
            model: valid_cfg.model,
            timeout_secs: 0,
        };
        assert!(zero_timeout.verify_config_invariants().is_err());

        let valid_req = request();
        assert!(valid_req.verify_request_invariants().is_ok());

        let with_hint = AuthorRequest {
            retry_hint: Some("fix scope overlap".to_string()),
            ..request()
        };
        assert!(with_hint.verify_request_invariants().is_ok());

        let blank_vector = AuthorRequest {
            vector: " ".to_string(),
            ..request()
        };
        assert!(blank_vector.verify_request_invariants().is_err());

        let blank_target = AuthorRequest {
            target: String::new(),
            ..request()
        };
        assert!(blank_target.verify_request_invariants().is_err());

        let empty_packages = AuthorRequest {
            packages: Vec::new(),
            ..request()
        };
        assert!(empty_packages.verify_request_invariants().is_err());

        let blank_package_entry = AuthorRequest {
            packages: vec!["susi-harness".to_string(), "  ".to_string()],
            ..request()
        };
        assert!(blank_package_entry.verify_request_invariants().is_err());

        let empty_workers = AuthorRequest {
            workers: Vec::new(),
            ..request()
        };
        assert!(empty_workers.verify_request_invariants().is_err());

        let blank_worker_entry = AuthorRequest {
            workers: vec![String::new()],
            ..request()
        };
        assert!(blank_worker_entry.verify_request_invariants().is_err());

        let zero_max_tasks = AuthorRequest {
            max_tasks: 0,
            ..request()
        };
        assert!(zero_max_tasks.verify_request_invariants().is_err());

        let blank_retry_hint = AuthorRequest {
            retry_hint: Some("   ".to_string()),
            ..request()
        };
        assert!(blank_retry_hint.verify_request_invariants().is_err());
    }
}
