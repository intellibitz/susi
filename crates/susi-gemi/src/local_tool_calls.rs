//! Extract tool calls emitted in-band by local GGUF models.
//!
//! Local engines receive a bare prompt — tool schemas are never rendered into
//! the chat template — but agent prompts routinely enumerate tools in prose,
//! and tool-capable models (see `ModelManager::supports_tool_calling`) answer
//! with their trained markup: `<tool_call>{...}</tool_call>` for Qwen/Hermes
//! dialects, `[TOOL_CALLS] [{...}]` for Mistral. Unhandled, that markup leaks
//! raw into the user-visible answer.
//!
//! `extract_tool_calls` separates the calls from the prose so the caller can
//! dispatch them through the same policy-gated `plane_bus::tools::execute_tool`
//! every other tool request uses. Each JSON payload is normalized via
//! `tool_call_normalisation::normalize_tool_call`, so OpenAI `{"function":
//! {...}}` and Anthropic/flat `{"name": ..., "arguments": ...}` shapes both
//! work. A delimited block that does not parse is kept in the prose —
//! undisciplined model output is evidence, not a call.

use crate::tool_call_normalisation::{normalize_tool_call, NormalizedToolCall};
use serde_json::Value;

/// Split generated text into prose and the tool calls it emitted.
/// Delimited blocks that fail to parse stay in the prose verbatim.
pub fn extract_tool_calls(text: &str) -> (String, Vec<NormalizedToolCall>) {
    let (mut prose, mut calls) = extract_tagged_calls(text, "<tool_call>", "</tool_call>");
    prose = extract_mistral_calls(&prose, &mut calls);
    (prose, calls)
}

/// Cap on tools enumerated for a local model — every schema costs context
/// tokens on models whose windows are small, and past a screenful the
/// enumeration confuses more than it helps.
const MAX_ENUMERATED_TOOLS: usize = 32;

/// Prepend the registered tool schemas to a prompt bound for a local
/// tool-capable model. There is no Jinja rendering — the engines receive a
/// bare string — so the catalog goes in as the `<tool_call>` emission
/// contract plus a `name — description` list, which is exactly the markup
/// `extract_tool_calls` reads back on the answer. Returns the prompt
/// untouched when nothing is registered (nothing for the model to call).
pub fn prompt_with_tool_schemas(prompt: &str) -> String {
    let tools = crate::susi_core::plane_bus::tools::list_tools();
    let Some(entries) = tools.as_array() else {
        return prompt.to_string();
    };
    let catalog: Vec<(&str, &str)> = entries
        .iter()
        .take(MAX_ENUMERATED_TOOLS)
        .filter_map(|tool| {
            Some((
                tool.get("name").and_then(Value::as_str)?,
                tool.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("no description"),
            ))
        })
        .collect();
    match tools_block(&catalog) {
        Some(block) => format!("{block}\n\n{prompt}"),
        None => prompt.to_string(),
    }
}

/// The tools preamble itself — `None` when the catalog is empty.
fn tools_block(catalog: &[(&str, &str)]) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    let lines: Vec<String> = catalog
        .iter()
        .map(|(name, description)| format!("- {name} — {description}"))
        .collect();
    Some(format!(
        "You may call any of these tools by replying with exactly\n\
         <tool_call>{{\"name\": \"<tool_name>\", \"arguments\": {{...}}}}</tool_call>\n\
         and nothing else inside the tag. Registered tools:\n{}",
        lines.join("\n")
    ))
}

/// Walk `<tool_call>...</tool_call>` pairs (case-insensitive; an unclosed
/// trailing block counts). Blocks whose inner JSON does not normalize are
/// preserved in the prose.
fn extract_tagged_calls(text: &str, open: &str, close: &str) -> (String, Vec<NormalizedToolCall>) {
    let lower = text.to_lowercase();
    let mut prose = String::with_capacity(text.len());
    let mut calls = Vec::new();
    let mut pos = 0;
    while let Some(start) = lower[pos..].find(open).map(|i| pos + i) {
        let inner_start = start + open.len();
        let (inner, block_end) = match lower[inner_start..].find(close) {
            Some(i) => (
                &text[inner_start..inner_start + i],
                inner_start + i + close.len(),
            ),
            None => (&text[inner_start..], text.len()),
        };
        match serde_json::from_str::<Value>(inner.trim())
            .ok()
            .and_then(|v| normalize_tool_call(&v))
        {
            Some(call) => {
                prose.push_str(&text[pos..start]);
                calls.push(call);
            }
            None => prose.push_str(&text[pos..block_end]),
        }
        pos = block_end;
    }
    prose.push_str(&text[pos..]);
    (prose, calls)
}

/// Mistral emits `[TOOL_CALLS]` followed by a JSON array of call objects,
/// typically at the end of the output. The array is located by balanced-bracket
/// scanning so trailing prose does not defeat the parse.
fn extract_mistral_calls(text: &str, calls: &mut Vec<NormalizedToolCall>) -> String {
    let lower = text.to_lowercase();
    let Some(marker) = lower.find("[tool_calls]") else {
        return text.to_string();
    };
    let after = &text[marker + "[tool_calls]".len()..];
    let ws = after.len() - after.trim_start().len();
    let arr_open = marker + "[tool_calls]".len() + ws;
    if !text[arr_open..].starts_with('[') {
        return text.to_string();
    }
    // The matching ']' lives `end` chars into the array's contents.
    let Some(end) = balanced_json_span(&text[arr_open + 1..]) else {
        return text.to_string();
    };
    let close = arr_open + end + 2; // one past the ']'
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text[arr_open..close]) else {
        return text.to_string();
    };
    let parsed: Vec<NormalizedToolCall> = items.iter().filter_map(normalize_tool_call).collect();
    if parsed.is_empty() {
        return text.to_string();
    }
    calls.extend(parsed);
    let mut prose = String::with_capacity(text.len());
    prose.push_str(&text[..marker]);
    prose.push_str(&text[close..]);
    prose
}

/// Given a string starting just after an opening `[`, return the end offset
/// of the balanced `]` — string literals and escapes respected, so brackets
/// inside JSON string values cannot unbalance the scan.
fn balanced_json_span(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => depth += 1,
            ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod local_tool_call_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_tool_calls_qwen_block_extracts_and_strips() {
        let text = "Let me look that up.\n<tool_call>\n{\"name\": \"search\", \"arguments\": {\"q\": \"susi\"}}\n</tool_call>\n";
        let (prose, calls) = extract_tool_calls(text);
        assert_eq!(prose.trim(), "Let me look that up.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].arguments, json!({"q": "susi"}));
    }

    #[test]
    fn local_tool_calls_openai_function_shape() {
        let text = "<tool_call>{\"function\": {\"name\": \"calc\", \"arguments\": {\"x\": 1}}}</tool_call>";
        let (prose, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "calc");
        assert!(prose.trim().is_empty());
    }

    #[test]
    fn local_tool_calls_multiple_blocks() {
        let text = "a<tool_call>{\"name\": \"one\"}</tool_call>b<tool_call>{\"name\": \"two\"}</tool_call>c";
        let (prose, calls) = extract_tool_calls(text);
        assert_eq!(prose, "abc");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].name, "two");
    }

    #[test]
    fn local_tool_calls_unclosed_trailing_block() {
        let text = "prose\n<tool_call>{\"name\": \"end_call\", \"arguments\": {}}";
        let (prose, calls) = extract_tool_calls(text);
        assert_eq!(prose.trim(), "prose");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "end_call");
    }

    #[test]
    fn local_tool_calls_mistral_array_with_trailing_prose() {
        let text = "Thinking… [TOOL_CALLS] [{\"name\": \"weather\", \"arguments\": {\"city\": \"Lisbon\"}}] done";
        let (prose, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "weather");
        assert_eq!(prose, "Thinking…  done");
    }

    #[test]
    fn local_tool_calls_malformed_block_stays_in_prose() {
        let text = "x<tool_call>not json at all</tool_call>y";
        let (prose, calls) = extract_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(prose, text);
    }

    #[test]
    fn local_tool_calls_plain_text_untouched() {
        let text = "An ordinary answer with <tags> and [brackets].";
        let (prose, calls) = extract_tool_calls(text);
        assert_eq!(prose, text);
        assert!(calls.is_empty());
    }

    #[test]
    fn local_tool_calls_brackets_inside_strings_do_not_unbalance() {
        let text = "[TOOL_CALLS] [{\"name\": \"echo\", \"arguments\": {\"s\": \"]] [\"}}]";
        let (_, calls) = extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["s"], "]] [");
    }

    /// The injected preamble teaches the exact markup the extractor reads
    /// back — a model that complies emits a call `extract_tool_calls` parses.
    #[test]
    fn local_tool_schemas_block_roundtrips_through_extraction() {
        let block = tools_block(&[
            ("search", "Search the workspace"),
            ("sql_query", "Run a SQL query"),
        ])
        .expect("non-empty catalog produces a block");
        assert!(block.contains("search — Search the workspace"));
        assert!(block.contains("<tool_call>"));
        // What the contract instructs is what the extractor accepts.
        let emitted =
            "<tool_call>{\"name\": \"search\", \"arguments\": {\"q\": \"x\"}}</tool_call>";
        let (_, calls) = extract_tool_calls(emitted);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
    }

    #[test]
    fn local_tool_schemas_block_none_when_empty() {
        assert!(tools_block(&[]).is_none());
        // Hermetic: no tools registered through the plane bus in tests.
        assert_eq!(prompt_with_tool_schemas("plain"), "plain");
    }
}
