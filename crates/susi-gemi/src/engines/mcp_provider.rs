//! Bridge MCP servers that expose LLM/chat tools into the `Provider` registry
//! so they participate in cloud routing, prefer, and slow-local escalation.

use std::any::Any;

use serde_json::json;
use susi_core::provider::{BoxFuture, Provider};
use susi_core::registry::CapabilityRegistry;
use susi_error::{EaiError, EaiResult};
use susi_tools::GmcpClient;

/// Inference backend backed by an MCP tool (`server:tool`).
pub struct McpInferenceProvider {
    pub name: String,
    pub server_name: String,
    pub tool_name: String,
}

impl McpInferenceProvider {
    pub fn new(server_name: impl Into<String>, tool_name: impl Into<String>) -> Self {
        let server_name = server_name.into();
        let tool_name = tool_name.into();
        let name = format!(
            "mcp-{}-{}",
            server_name.to_ascii_lowercase().replace(' ', "-"),
            tool_name.to_ascii_lowercase().replace(' ', "-")
        );
        Self {
            name,
            server_name,
            tool_name,
        }
    }
}

impl Provider for McpInferenceProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        // Presence in the live MCP catalog is the health signal; probing every
        // generate would be too expensive for the router.
        Box::pin(async move { Ok(true) })
    }

    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        let server = self.server_name.clone();
        let tool = self.tool_name.clone();
        let prompt = prompt.to_string();
        Box::pin(async move {
            let text = invoke_mcp_llm(&server, &tool, &prompt);
            if text.trim().is_empty() || text.contains("[FAIL]") || text.contains("MCP Error") {
                return Err(EaiError::inference(format!(
                    "MCP inference via {}:{} failed: {}",
                    server,
                    tool,
                    text.chars().take(240).collect::<String>()
                )));
            }
            Ok(text)
        })
    }

    fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        Box::pin(async move {
            Err(EaiError::inference(
                "MCP inference providers do not expose embeddings",
            ))
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Call an MCP LLM-shaped tool with a few common argument layouts.
fn invoke_mcp_llm(server: &str, tool: &str, prompt: &str) -> String {
    // Prefer structured JSON so MCP servers with typed schemas get a prompt.
    let payloads = [
        json!({ "prompt": prompt }).to_string(),
        json!({ "message": prompt }).to_string(),
        json!({ "input": prompt }).to_string(),
        json!({
            "messages": [{ "role": "user", "content": prompt }]
        })
        .to_string(),
        prompt.to_string(),
    ];
    let mut last = String::new();
    for payload in payloads {
        let res = GmcpClient::execute_external_tool(server, tool, &payload);
        last = res.clone();
        if !res.contains("[FAIL]") && !res.contains("MCP Error") && !res.trim().is_empty() {
            return extract_text_payload(&res);
        }
    }
    last
}

fn extract_text_payload(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(s) = v.as_str() {
            return s.to_string();
        }
        for key in ["text", "content", "output", "result", "message", "response"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                return s.to_string();
            }
        }
        if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
            let mut out = String::new();
            for item in arr {
                if let Some(s) = item.get("text").and_then(|t| t.as_str()) {
                    out.push_str(s);
                } else if let Some(s) = item.as_str() {
                    out.push_str(s);
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    raw.to_string()
}

/// True when an MCP tool looks like a chat / completion / reasoning backend
/// rather than a generic capability (search, git, sql, …).
pub fn looks_like_inference_tool(server: &str, tool: &str, description: &str) -> bool {
    let blob = format!("{} {} {}", server, tool, description).to_ascii_lowercase();

    const NEGATIVE: &[&str] = &[
        "postgres",
        "sqlite",
        "mysql",
        "database",
        "sql ",
        "github",
        "gitlab",
        "browser",
        "puppeteer",
        "filesystem",
        "file_read",
        "file_write",
        "search_web",
        "web_search",
        "fetch_url",
        "http_request",
        "email",
        "calendar",
        "slack",
        "discord",
    ];
    if NEGATIVE.iter().any(|n| blob.contains(n)) {
        // Still allow if the tool itself is clearly generative.
        let tool_l = tool.to_ascii_lowercase();
        if !(tool_l.contains("chat")
            || tool_l.contains("complet")
            || tool_l.contains("generate")
            || tool_l == "reason"
            || tool_l.contains("llm"))
        {
            return false;
        }
    }

    // Registry-classified intelligence / reasoning servers (see scout_reasoning_remotes).
    if GmcpClient::scout_reasoning_remotes()
        .iter()
        .any(|s| s.eq_ignore_ascii_case(server))
    {
        return true;
    }

    const POSITIVE: &[&str] = &[
        "chat",
        "completion",
        "complet",
        "generate",
        "inference",
        "infer",
        "language model",
        "llm",
        "prompt",
        "reason",
        "ask",
        "openai",
        "anthropic",
        "claude",
        "gemini",
        "deepseek",
        "minimax",
        "moonshot",
        "kimi",
        "groq",
        "mistral",
        "together",
        "fireworks",
        "openrouter",
    ];
    POSITIVE.iter().any(|p| blob.contains(p))
}

/// Discover live MCP tools and register LLM-shaped ones as `Provider`s.
pub fn register_mcp_inference_providers(registry: &CapabilityRegistry) {
    let live = GmcpClient::discover_live_tools();
    let candidates: Vec<(String, String, String)> = if live.is_empty() {
        // Config-only: wildcard proxies aren't callable as chat — skip.
        Vec::new()
    } else {
        live.into_iter()
            .filter_map(|(server, tool)| {
                let mcp_tool = tool
                    .name
                    .split_once(':')
                    .map(|(_, t)| t.to_string())
                    .unwrap_or_else(|| tool.name.clone());
                if looks_like_inference_tool(&server, &mcp_tool, &tool.description) {
                    Some((server, mcp_tool, tool.description))
                } else {
                    None
                }
            })
            .collect()
    };

    for (server, tool, _desc) in candidates {
        let provider = McpInferenceProvider::new(&server, &tool);
        if registry.get_provider(&provider.name).is_some() {
            continue;
        }
        if std::env::var("SUSI_VERBOSE").is_ok() {
            eprintln!(
                "[AUTODISCOVER] Registered MCP inference provider: {} ({}:{})",
                provider.name, server, tool
            );
        }
        registry.register_provider(provider);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_heuristic_accepts_chat_tools() {
        assert!(looks_like_inference_tool(
            "openai-bridge",
            "chat",
            "Chat completion via OpenAI"
        ));
        assert!(looks_like_inference_tool(
            "my-llm",
            "generate",
            "Generate text with a local gateway"
        ));
        assert!(looks_like_inference_tool(
            "brain",
            "reason",
            "Reasoning pulse"
        ));
    }

    #[test]
    fn inference_heuristic_rejects_data_tools() {
        assert!(!looks_like_inference_tool(
            "database",
            "query",
            "Run SQL against postgres"
        ));
        assert!(!looks_like_inference_tool(
            "github",
            "create_issue",
            "Create a GitHub issue"
        ));
        assert!(!looks_like_inference_tool(
            "search",
            "web_search",
            "Search the web"
        ));
    }

    #[test]
    fn extract_text_from_mcp_json_shapes() {
        assert_eq!(extract_text_payload(r#"{"text":"hello"}"#), "hello");
        assert_eq!(
            extract_text_payload(r#"{"content":[{"type":"text","text":"hi"}]}"#),
            "hi"
        );
        assert_eq!(extract_text_payload("plain"), "plain");
    }

    #[test]
    fn provider_name_is_stable() {
        let p = McpInferenceProvider::new("OpenAI Bridge", "Chat Completions");
        assert_eq!(p.name(), "mcp-openai-bridge-chat-completions");
    }
}
