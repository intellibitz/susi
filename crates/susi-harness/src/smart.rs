//! Full-cache, full-budget reasoning, infinite-thinking continuation, and
//! Astra/Opus-level autonomous worker intelligence for `susi-harness`.
//!
//! This module translates the three pillars of Susi Harness intelligence into
//! deterministic runtime primitives used by [`crate::worker`], [`crate::author`],
//! and [`crate::gateway`]:
//!
//! 1. **Full Cache On**: canonical prefix-stable message ordering, deterministic
//!    per-task cache scope keys (`x-susi-cache-scope`), and multi-turn prompt
//!    cache reuse across all turns of a worker session.
//! 2. **Full Budget Reasoning & Infinite Thinking**: model-optimal reasoning
//!    parameters (`reasoning_effort = "high"`, `thinking = {"type": "enabled"}`,
//!    and generous `16_384`–`32_768` output token headroom) plus multi-turn
//!    continuation when a reasoning or generation turn stops with
//!    `finish_reason == "length"`.
//! 3. **AI Smartness Built In**: phase-aware parameter scaling (`InitialPlanning`,
//!    `ActiveExecution`, `ErrorRecovery`, `InfiniteThinkingContinuation`) and
//!    self-healing loop/error detection that guides the worker out of repeated
//!    tool failures.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Standard output token ceiling for an active worker turn so hidden thinking
/// never starves tool-call arguments or code patches.
pub const DEFAULT_REASONING_MAX_TOKENS: u64 = 16_384;

/// Elevated output token ceiling for initial planning, rescue/escalation
/// attempts (`attempt > 1`), error recovery, and frontier reasoning models.
pub const ELEVATED_REASONING_MAX_TOKENS: u64 = 24_576;

/// Maximum per-turn output token ceiling used when stepping up during
/// multi-turn infinite-thinking continuation.
pub const MAX_INFINITE_THINKING_TOKENS: u64 = 32_768;

/// Maximum consecutive `finish_reason == "length"` continuations before the
/// worker requires a concrete tool action or final conclusion.
pub const MAX_CONTINUATION_STREAK: u32 = 6;

/// Reasoning effort level synthesized for a worker or author turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Balanced reasoning depth.
    Medium,
    /// Full-budget reasoning depth (default for Susi Harness).
    High,
    /// Maximum reasoning depth for rescue attempts, error recovery, and
    /// multi-turn infinite-thinking continuation.
    Max,
}

impl ReasoningEffort {
    /// Wire value for OpenAI / Gemini / OpenRouter `reasoning_effort`.
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::High | Self::Max => "high",
        }
    }
}

/// Execution phase of the current worker turn, used to adapt reasoning depth
/// and token headroom dynamically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    /// First turn of a task: architectural analysis, file discovery, and planning.
    InitialPlanning,
    /// Standard tool-use execution turn.
    ActiveExecution,
    /// Turn immediately following a tool error, compiler/test failure, or
    /// repeated tool-call loop.
    ErrorRecovery,
    /// Turn continuing a thought stream or draft that hit `finish_reason == "length"`.
    InfiniteThinkingContinuation {
        /// Consecutive continuation index (1-based).
        depth: u32,
    },
}

/// Synthesized per-turn inference parameters for full cache and full-budget reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartParams {
    /// Output token ceiling (`max_tokens`) for the turn.
    pub max_tokens: u64,
    /// Reasoning effort level (`Some("high")` for models supporting `reasoning_effort`).
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether Moonshot/Kimi `"thinking": {"type": "enabled"}` should be attached.
    pub enable_thinking_object: bool,
    /// Deterministic cache scope identifier for prompt/response cache affinity.
    pub cache_scope: String,
    /// Phase that produced these parameters.
    pub phase: TurnPhase,
}

impl SmartParams {
    /// Synthesize optimal full-cache, full-budget reasoning parameters for
    /// `model` on `attempt` (1-based) in `phase`.
    #[must_use]
    pub fn for_turn(
        model: &str,
        mission: &str,
        task: &str,
        attempt: u32,
        phase: TurnPhase,
    ) -> Self {
        let lower = model.to_ascii_lowercase();
        let is_frontier_or_rescue = attempt > 1
            || lower.contains("opus")
            || lower.contains("sonnet")
            || lower.contains("pro")
            || lower.contains("reasoner")
            || lower.contains("gpt-sol")
            || lower.contains("o1")
            || lower.contains("o3")
            || lower.contains("o4")
            || lower.contains("k3")
            || lower.contains("coder");

        let max_tokens = match phase {
            TurnPhase::InfiniteThinkingContinuation { depth } => {
                if depth >= 2 || is_frontier_or_rescue {
                    MAX_INFINITE_THINKING_TOKENS
                } else {
                    ELEVATED_REASONING_MAX_TOKENS
                }
            }
            TurnPhase::InitialPlanning | TurnPhase::ErrorRecovery => {
                if is_frontier_or_rescue {
                    MAX_INFINITE_THINKING_TOKENS
                } else {
                    ELEVATED_REASONING_MAX_TOKENS
                }
            }
            TurnPhase::ActiveExecution => {
                if is_frontier_or_rescue {
                    ELEVATED_REASONING_MAX_TOKENS
                } else {
                    DEFAULT_REASONING_MAX_TOKENS
                }
            }
        };

        let effort = match phase {
            TurnPhase::InitialPlanning
            | TurnPhase::ErrorRecovery
            | TurnPhase::InfiniteThinkingContinuation { .. } => ReasoningEffort::Max,
            TurnPhase::ActiveExecution if attempt > 1 || is_frontier_or_rescue => {
                ReasoningEffort::Max
            }
            TurnPhase::ActiveExecution => ReasoningEffort::High,
        };

        let supports_reasoning_effort = lower.starts_with("gemini")
            || lower.contains("/gemini")
            || lower.starts_with("or-gemini")
            || lower.starts_with("gpt-sol")
            || lower.starts_with("or-gpt-sol")
            || lower.starts_with("o1")
            || lower.starts_with("o3")
            || lower.starts_with("o4");

        let enable_thinking_object = lower.contains("kimi") || lower.contains("moonshot");

        Self {
            max_tokens,
            reasoning_effort: supports_reasoning_effort.then_some(effort),
            enable_thinking_object,
            cache_scope: cache_scope_key(mission, task, attempt),
            phase,
        }
    }

    /// Apply these synthesized parameters to an outbound OpenAI-compatible
    /// `/v1/chat/completions` JSON request body.
    pub fn apply_to_request(&self, body: &mut Value) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        obj.insert("max_tokens".to_string(), Value::from(self.max_tokens));
        if let Some(effort) = self.reasoning_effort {
            obj.insert(
                "reasoning_effort".to_string(),
                Value::from(effort.as_wire_str()),
            );
        }
        if self.enable_thinking_object {
            obj.insert("thinking".to_string(), json!({"type": "enabled"}));
        }
    }
}

/// Deterministic cache scope identifier for a `(mission, task, attempt)` tuple.
#[must_use]
pub fn cache_scope_key(mission: &str, task: &str, attempt: u32) -> String {
    format!("susi-cache:{mission}:{task}:a{attempt}")
}

/// How the worker loop should handle a model reply that contained no tool calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnDisposition {
    /// The model hit `finish_reason == "length"` while thinking or generating
    /// and should be given a seamless continuation turn ("infinite thinking").
    ContinueThinking {
        /// Partial reasoning trace emitted in this turn, if any.
        reasoning: String,
        /// Partial assistant text emitted in this turn, if any.
        partial_text: String,
        /// Prompt injected to continue seamlessly without losing state.
        continuation_prompt: String,
    },
    /// The model emitted a complete final answer and concluded the task.
    Conclude {
        /// Final assistant answer text.
        final_text: String,
        /// Reasoning trace emitted on the final turn, if any.
        reasoning: String,
    },
    /// The model returned an genuinely empty turn with no tool calls, no text,
    /// and no length-truncated reasoning.
    EmptyTurn,
}

/// Extract any reasoning/thinking trace (`reasoning_content` or `thinking`)
/// from an assistant message object.
#[must_use]
pub fn extract_reasoning_trace(message: &Value) -> String {
    if let Some(r) = message.get("reasoning_content").and_then(Value::as_str) {
        let trimmed = r.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(r) = message.get("thinking").and_then(Value::as_str) {
        let trimmed = r.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(r) = message.get("reasoning").and_then(Value::as_str) {
        let trimmed = r.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    String::new()
}

/// Classify a non-tool-call assistant turn (`choice`) to decide whether to
/// chain an **Infinite Thinking** continuation turn, conclude with the final
/// answer, or record an empty turn.
#[must_use]
pub fn classify_non_tool_turn(choice: &Value, continuation_streak: u32) -> TurnDisposition {
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let message = &choice["message"];
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reasoning = extract_reasoning_trace(message);
    let has_text = !text.trim().is_empty();
    let has_reasoning = !reasoning.is_empty();

    if finish_reason == "length"
        && (has_reasoning || has_text)
        && continuation_streak < MAX_CONTINUATION_STREAK
    {
        let continuation_prompt = if has_text {
            "Your previous response reached the single-turn output token limit (`finish_reason: length`) before finishing. Continue seamlessly from where you stopped and either issue the next tool call or complete your final conclusion.".to_string()
        } else {
            "Your previous turn spent its single-turn token budget on deep reasoning (`finish_reason: length`) before emitting a tool call or conclusion. Continue from your reasoning above and emit the next tool call or final answer directly.".to_string()
        };
        return TurnDisposition::ContinueThinking {
            reasoning,
            partial_text: text,
            continuation_prompt,
        };
    }

    if has_text {
        return TurnDisposition::Conclude {
            final_text: text,
            reasoning,
        };
    }

    // Even if `finish_reason` was not `"length"`, a model that emitted only
    // `reasoning_content` and empty `content` on a turn is mid-thought rather
    // than dead, so allow one continuation before counting it as empty.
    if has_reasoning && continuation_streak == 0 {
        return TurnDisposition::ContinueThinking {
            reasoning,
            partial_text: String::new(),
            continuation_prompt: "You emitted reasoning without a tool call or final response. Proceed with the next tool call or state your final conclusion.".to_string(),
        };
    }

    TurnDisposition::EmptyTurn
}

/// Self-healing tracker that monitors tool calls and tool outcomes across a
/// worker's multi-turn loop to detect repeated identical calls or consecutive
/// errors and synthesize recovery guidance.
#[derive(Debug, Default, Clone)]
pub struct SmartAdvisor {
    last_call_signature: Option<(String, String)>,
    repeat_streak: u32,
    consecutive_errors: u32,
    total_tool_errors: u32,
}

impl SmartAdvisor {
    /// Create a new advisor for a worker session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current turn phase recommended by the advisor given `turn` (0-based)
    /// and `continuation_streak`.
    #[must_use]
    pub fn recommend_phase(&self, turn: usize, continuation_streak: u32) -> TurnPhase {
        if continuation_streak > 0 {
            return TurnPhase::InfiniteThinkingContinuation {
                depth: continuation_streak,
            };
        }
        if self.consecutive_errors > 0 || self.repeat_streak >= 2 {
            return TurnPhase::ErrorRecovery;
        }
        if turn == 0 {
            return TurnPhase::InitialPlanning;
        }
        TurnPhase::ActiveExecution
    }

    /// Record a tool invocation and its outcome (`is_error`), returning an
    /// optional self-healing diagnostic nudge to append to the tool result when
    /// a loop or repeated failure is detected.
    pub fn observe_tool_call(
        &mut self,
        tool_name: &str,
        arguments: &str,
        output: &str,
    ) -> Option<String> {
        let normalized_args = arguments.trim().to_string();
        let signature = (tool_name.to_string(), normalized_args);
        if self.last_call_signature.as_ref() == Some(&signature) {
            self.repeat_streak = self.repeat_streak.saturating_add(1);
        } else {
            self.last_call_signature = Some(signature);
            self.repeat_streak = 1;
        }

        let is_error = output.starts_with("Error executing ")
            || output.contains("Command failed (exit ")
            || output.starts_with("Unknown tool:");

        if is_error {
            self.consecutive_errors = self.consecutive_errors.saturating_add(1);
            self.total_tool_errors = self.total_tool_errors.saturating_add(1);
        } else {
            self.consecutive_errors = 0;
        }

        if self.repeat_streak >= 2 && is_error {
            Some(format!(
                "\n\n[Susi Smartness Advisor]: Tool `{tool_name}` was called {} times in a row with identical arguments and failed. Stop repeating the identical call: inspect the error details above, check file paths or syntax with `read_file`/`list_dir`, and apply a different fix.",
                self.repeat_streak
            ))
        } else if self.repeat_streak >= 3 {
            Some(format!(
                "\n\n[Susi Smartness Advisor]: Tool `{tool_name}` has been invoked {} consecutive times with identical arguments. Avoid redundant calls—advance to the next step of the task or conclude if the goal is met.",
                self.repeat_streak
            ))
        } else if self.consecutive_errors >= 2 {
            Some(format!(
                "\n\n[Susi Smartness Advisor]: {} consecutive tool errors occurred. Pause and diagnose the root cause before making another edit or command call.",
                self.consecutive_errors
            ))
        } else {
            None
        }
    }

    /// Total tool errors observed during this attempt.
    #[must_use]
    pub const fn total_tool_errors(&self) -> u32 {
        self.total_tool_errors
    }
}

/// System prompt for the autonomous multi-turn worker, ordered with a static
/// canonical prefix so provider prompt caches hit on every turn.
#[must_use]
pub const fn worker_system_prompt() -> &'static str {
    "You are an autonomous Astra/Opus-grade software engineering agent executing a task in a workspace.\n\
     Operate with full engineering ownership and precision:\n\
     1. Understand before acting: use `read_file` or `list_dir` to inspect target files and existing contracts before modifying code.\n\
     2. Surgical execution: use `replace_in_file` for targeted edits or `write_to_file` for new files, preserving unrelated comments and interfaces.\n\
     3. Self-healing verification: use `run_command` to verify builds/tests when appropriate, and if a tool or command fails, diagnose the root cause instead of repeating the same failing call.\n\
     4. Conclude cleanly: when the task is complete, stop calling tools and provide a concise summary of the verified changes."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_and_reasoning_models_receive_full_budget_high_reasoning_never_low() {
        let params = SmartParams::for_turn(
            "gemini-flash",
            "m-smart",
            "t-1",
            1,
            TurnPhase::InitialPlanning,
        );
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::Max));
        assert_eq!(
            params.reasoning_effort.unwrap().as_wire_str(),
            "high",
            "Gemini must never be throttled to low reasoning_effort"
        );
        assert!(params.max_tokens >= ELEVATED_REASONING_MAX_TOKENS);
        assert_eq!(params.cache_scope, "susi-cache:m-smart:t-1:a1");

        let mut body = json!({
            "model": "gemini-flash",
            "messages": [{"role": "user", "content": "solve"}]
        });
        params.apply_to_request(&mut body);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["max_tokens"], ELEVATED_REASONING_MAX_TOKENS);
    }

    #[test]
    fn rescue_and_frontier_models_scale_to_maximum_token_headroom() {
        let opus =
            SmartParams::for_turn("claude-opus", "m-smart", "t-2", 2, TurnPhase::ErrorRecovery);
        assert_eq!(opus.max_tokens, MAX_INFINITE_THINKING_TOKENS);
        assert!(
            opus.reasoning_effort.is_none(),
            "Anthropic native wire uses thinking blocks rather than OpenAI reasoning_effort"
        );

        let kimi = SmartParams::for_turn(
            "kimi-k3-pro",
            "m-smart",
            "t-2",
            1,
            TurnPhase::ActiveExecution,
        );
        assert!(kimi.enable_thinking_object);
        assert_eq!(kimi.max_tokens, ELEVATED_REASONING_MAX_TOKENS);
        let mut body = json!({"model": "kimi-k3-pro"});
        kimi.apply_to_request(&mut body);
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn length_truncated_reasoning_or_output_triggers_infinite_thinking_continuation() {
        let choice_reasoning_only = json!({
            "finish_reason": "length",
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "Analyzing module dependencies and lifetime bounds..."
            }
        });
        match classify_non_tool_turn(&choice_reasoning_only, 0) {
            TurnDisposition::ContinueThinking {
                reasoning,
                partial_text,
                continuation_prompt,
            } => {
                assert!(reasoning.contains("Analyzing module dependencies"));
                assert!(partial_text.is_empty());
                assert!(continuation_prompt.contains("finish_reason: length"));
            }
            other => panic!("expected ContinueThinking, got {other:?}"),
        }

        // Once MAX_CONTINUATION_STREAK is reached, a partial text concludes
        // rather than looping forever, and an empty text becomes EmptyTurn.
        let exhausted = classify_non_tool_turn(&choice_reasoning_only, MAX_CONTINUATION_STREAK);
        assert_eq!(exhausted, TurnDisposition::EmptyTurn);
    }

    #[test]
    fn smart_advisor_detects_repeated_failing_calls_and_escalates_to_error_recovery() {
        let mut advisor = SmartAdvisor::new();
        assert_eq!(advisor.recommend_phase(0, 0), TurnPhase::InitialPlanning);
        assert_eq!(advisor.recommend_phase(1, 0), TurnPhase::ActiveExecution);

        let first = advisor.observe_tool_call(
            "replace_in_file",
            r#"{"path":"src/lib.rs"}"#,
            "Error executing replace_in_file: SEARCH block not found",
        );
        assert!(
            first.is_none(),
            "single error triggers ErrorRecovery phase without noisy banner"
        );
        assert_eq!(advisor.recommend_phase(2, 0), TurnPhase::ErrorRecovery);

        let second = advisor.observe_tool_call(
            "replace_in_file",
            r#"{"path":"src/lib.rs"}"#,
            "Error executing replace_in_file: SEARCH block not found",
        );
        assert!(
            second
                .as_deref()
                .is_some_and(|msg| msg.contains("Susi Smartness Advisor")),
            "repeated failing call must inject self-healing recovery guidance"
        );
        assert_eq!(advisor.total_tool_errors(), 2);
    }

    #[test]
    fn extract_reasoning_trace_falls_back_across_fields_and_concludes_complete_turn() {
        assert_eq!(
            extract_reasoning_trace(&json!({"thinking": "  step 1  "})),
            "step 1"
        );
        assert_eq!(
            extract_reasoning_trace(&json!({"reasoning": "  step 2  "})),
            "step 2"
        );
        assert!(extract_reasoning_trace(&json!({"content": "only text"})).is_empty());

        let stop_choice = json!({
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "Verified and done.",
                "thinking": "Checked tests"
            }
        });
        assert_eq!(
            classify_non_tool_turn(&stop_choice, 0),
            TurnDisposition::Conclude {
                final_text: "Verified and done.".to_string(),
                reasoning: "Checked tests".to_string(),
            }
        );
        assert!(worker_system_prompt().contains("autonomous Astra/Opus-grade"));
    }
}
