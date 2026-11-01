//! Direct streaming model execution CLI (`susi run`).
//!
//! Provides fast, single-turn completion against local models (Ollama,
//! llama.cpp, Candle) or configured cloud providers without the overhead
//! of swarm DAG compilation.

use clap::Args;
use serde::Serialize;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use susi_gemi::engines::runtime::GemiEngine;

/// Arguments for `susi run`.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Model name (e.g. `qwen2.5-coder:7b`, `claude-3-7-sonnet`, `deepseek-chat`)
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// System prompt / instructions
    #[arg(short = 's', long = "system")]
    pub system: Option<String>,

    /// Disable streaming output (buffers complete response before printing)
    #[arg(long = "no-stream")]
    pub no_stream: bool,

    /// Output JSON with metadata (duration, token throughput, serving backend)
    #[arg(long = "json")]
    pub json: bool,

    /// Positional model name and/or prompt tokens
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub prompt: Vec<String>,
}

#[derive(Serialize)]
pub struct RunJsonOutput<'a> {
    pub model: &'a str,
    pub response: &'a str,
    pub tokens: usize,
    pub duration_ms: u128,
    pub tok_per_sec: f64,
}

/// Heuristic to detect if a CLI token is intended as a model identifier
/// rather than starting a prompt sentence.
pub fn looks_like_model(token: &str) -> bool {
    let t = token.trim();
    if t.is_empty() {
        return false;
    }
    // Tagged local model names: `qwen2.5-coder:7b`, `llama3.2:3b`, etc.
    if t.contains(':') && !t.contains(' ') {
        return true;
    }
    // HuggingFace / registry paths: `meta-llama/Llama-3-8B`, etc.
    if t.contains('/') && !t.contains(' ') {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    // Hyphenated cloud or open-weight model families
    if lower.starts_with("claude-")
        || lower.starts_with("gpt-")
        || lower.starts_with("gemini-")
        || lower.starts_with("deepseek-")
        || lower.starts_with("phi-")
        || lower.starts_with("phi3")
        || lower.starts_with("phi4")
        || lower.starts_with("yi-")
        || lower.starts_with("codellama")
    {
        return true;
    }
    // Common local model prefixes with a version/size suffix (e.g. qwen2.5, llama3, mistral-7b)
    if (lower.starts_with("qwen")
        || lower.starts_with("llama")
        || lower.starts_with("mistral")
        || lower.starts_with("gemma"))
        && (t.chars().any(|c| c.is_ascii_digit()) || t.contains('-'))
    {
        return true;
    }
    // Exact known provider names or engine aliases
    matches!(
        lower.as_str(),
        "ollama"
            | "llamacpp"
            | "candle"
            | "openai"
            | "anthropic"
            | "gemini"
            | "deepseek"
            | "openrouter"
            | "groq"
            | "together"
            | "fireworks"
    )
}

/// Resolves explicit or positional model and combines prompt tokens with stdin if present.
pub fn resolve_model_and_prompt(
    cli_model: Option<String>,
    prompt_tokens: &[String],
    stdin_input: Option<String>,
) -> anyhow::Result<(Option<String>, String)> {
    let mut model = cli_model;
    let user_text = if model.is_some() {
        prompt_tokens.join(" ")
    } else if prompt_tokens.is_empty() {
        String::new()
    } else if prompt_tokens.len() == 1 {
        let first = &prompt_tokens[0];
        if stdin_input.is_some() && looks_like_model(first) {
            model = Some(first.clone());
            String::new()
        } else {
            first.clone()
        }
    } else {
        // prompt_tokens.len() >= 2
        let first = &prompt_tokens[0];
        if looks_like_model(first) {
            model = Some(first.clone());
            prompt_tokens[1..].join(" ")
        } else {
            prompt_tokens.join(" ")
        }
    };

    let user_trimmed = user_text.trim();
    let full_prompt = match (user_trimmed.is_empty(), stdin_input) {
        (false, Some(s)) => format!("{user_trimmed}\n\n{s}"),
        (true, Some(s)) => s,
        (_, None) => user_trimmed.to_string(),
    };

    let trimmed = full_prompt.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("No prompt provided. Usage: susi run [model] \"<prompt>\"");
    }

    Ok((model, trimmed))
}

fn read_stdin_bounded() -> io::Result<Option<String>> {
    if io::stdin().is_terminal() {
        return Ok(None);
    }
    let stdin = io::stdin();
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let max_size = cfg.max_stdin_size_bytes();
    let mut buffer = Vec::new();
    let mut limited = stdin.take((max_size as u64).saturating_add(1));
    limited.read_to_end(&mut buffer)?;
    if buffer.len() > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Input exceeds {} bytes limit", max_size),
        ));
    }
    let content =
        String::from_utf8(buffer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let trimmed = content.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    })
}

/// Rough estimation of tokens from output string:
/// Counts whitespace-separated words and scales by 1.33, minimum 1 if non-empty.
pub fn estimate_token_count(text: &str) -> usize {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let words = trimmed.split_whitespace().count();
    ((words as f64) * 1.33).ceil() as usize
}

/// Execute `susi run`.
pub fn execute(args: RunArgs, workspace: &Path) -> anyhow::Result<()> {
    let stdin_input = read_stdin_bounded()?;
    let (model, prompt) = resolve_model_and_prompt(args.model, &args.prompt, stdin_input)?;

    let full_prompt = match &args.system {
        Some(sys) if !sys.trim().is_empty() => {
            format!("System instructions:\n{}\n\nUser:\n{}", sys.trim(), prompt)
        }
        _ => prompt,
    };

    let serving_model = Arc::new(Mutex::new(
        model.clone().unwrap_or_else(|| "auto".to_string()),
    ));
    let serving_model_cb = Arc::clone(&serving_model);

    let stdout_lock = Arc::new(Mutex::new(io::stdout()));
    let stdout_cb = Arc::clone(&stdout_lock);

    let stream = !args.no_stream && !args.json;

    let start_time = Instant::now();

    let meta_callback = move |served: &str| {
        let mut m = serving_model_cb.lock().unwrap_or_else(|e| e.into_inner());
        *m = served.to_string();
    };

    let stream_callback = move |chunk: String| {
        if stream {
            let mut out = stdout_cb.lock().unwrap_or_else(|e| e.into_inner());
            let _ = out.write_all(chunk.as_bytes());
            let _ = out.flush();
        }
    };

    let response = GemiEngine::generate_reasoning_stream_deep_meta(
        &full_prompt,
        workspace,
        &stream_callback,
        model.as_deref(),
        &meta_callback,
    );

    // Engine adapters currently carry typed failures as marker text. A failed
    // goal must not become a successful process or a fabricated throughput row.
    validate_response(&response)?;

    let elapsed = start_time.elapsed();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let final_model = serving_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "unknown".to_string());

    let tokens = estimate_token_count(&response);
    let tok_per_sec = (tokens as f64) / elapsed_secs;

    if args.json {
        let json_obj = RunJsonOutput {
            model: &final_model,
            response: &response,
            tokens,
            duration_ms: elapsed.as_millis(),
            tok_per_sec: (tok_per_sec * 10.0).round() / 10.0,
        };
        println!("{}", serde_json::to_string_pretty(&json_obj)?);
    } else {
        if args.no_stream {
            println!("{response}");
        } else {
            // Ensure trailing newline after streaming
            if !response.ends_with('\n') {
                println!();
            }
        }

        // Status line on stderr when interactive TTY
        if io::stderr().is_terminal() {
            eprintln!(
                "[susi] Served by {} · ~{} tokens in {:.2}s ({:.1} tok/s)",
                final_model, tokens, elapsed_secs, tok_per_sec
            );
        }
    }

    Ok(())
}

fn validate_response(response: &str) -> anyhow::Result<()> {
    if response.trim().is_empty() {
        anyhow::bail!("No model returned an answer. Check available local or cloud connections.");
    }
    if GemiEngine::looks_like_error_text(response) {
        anyhow::bail!("Inference failed: {response}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_goal_inference_failure_is_not_success() {
        assert!(validate_response("[FAIL] SUSI-Tier2-Inference: no usable model").is_err());
        assert!(validate_response("[CAPABILITY_GAP] missing tool support").is_err());
        assert!(validate_response("  ").is_err());
        assert!(validate_response("READY").is_ok());
    }

    #[test]
    fn test_looks_like_model() {
        assert!(looks_like_model("qwen2.5-coder:7b"));
        assert!(looks_like_model("llama3.2:1b"));
        assert!(looks_like_model("meta-llama/Llama-3-8B"));
        assert!(looks_like_model("claude-3-7-sonnet"));
        assert!(looks_like_model("gpt-4o"));
        assert!(looks_like_model("deepseek-chat"));
        assert!(looks_like_model("qwen2.5"));
        assert!(looks_like_model("ollama"));

        // Plain words should not be treated as model
        assert!(!looks_like_model("hello"));
        assert!(!looks_like_model("why"));
        assert!(!looks_like_model("write a rust function"));
    }

    #[test]
    fn test_resolve_model_and_prompt_positional() {
        let tokens = vec![
            "qwen2.5-coder:7b".to_string(),
            "write".to_string(),
            "quicksort".to_string(),
        ];
        let (model, prompt) = resolve_model_and_prompt(None, &tokens, None).unwrap();
        assert_eq!(model.as_deref(), Some("qwen2.5-coder:7b"));
        assert_eq!(prompt, "write quicksort");
    }

    #[test]
    fn test_resolve_model_and_prompt_auto_placement() {
        let tokens = vec!["write".to_string(), "quicksort".to_string()];
        let (model, prompt) = resolve_model_and_prompt(None, &tokens, None).unwrap();
        assert_eq!(model, None);
        assert_eq!(prompt, "write quicksort");
    }

    #[test]
    fn test_resolve_model_and_prompt_with_explicit_flag() {
        let tokens = vec!["write".to_string(), "quicksort".to_string()];
        let (model, prompt) =
            resolve_model_and_prompt(Some("ollama/llama3".to_string()), &tokens, None).unwrap();
        assert_eq!(model.as_deref(), Some("ollama/llama3"));
        assert_eq!(prompt, "write quicksort");
    }

    #[test]
    fn test_resolve_model_and_prompt_stdin() {
        let tokens = vec!["qwen2.5:7b".to_string()];
        let (model, prompt) =
            resolve_model_and_prompt(None, &tokens, Some("quicksort input".to_string())).unwrap();
        assert_eq!(model.as_deref(), Some("qwen2.5:7b"));
        assert_eq!(prompt, "quicksort input");
    }

    #[test]
    fn test_token_estimation() {
        assert_eq!(estimate_token_count(""), 0);
        assert_eq!(estimate_token_count("hello world"), 3);
    }
}
