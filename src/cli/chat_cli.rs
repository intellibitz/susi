//! Interactive chat session CLI (`susi chat`).
//!
//! Provides a persistent, multi-turn conversational REPL against local or
//! cloud models with streaming tokens, slash commands (`/model`, `/tokens`,
//! `/clear`, `/system`, `/exit`), and real-time token/sec telemetry.

use clap::Args;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use susi_gemi::engines::runtime::GemiEngine;
use susi_gemi::models::ModelManager;

use super::run_cli::estimate_token_count;

/// Arguments for `susi chat`.
#[derive(Args, Debug, Clone)]
pub struct ChatArgs {
    /// Initial model to use (e.g. `qwen2.5-coder:7b`, `claude-3-7-sonnet`, `deepseek-chat`, or auto)
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Initial system prompt instructions
    #[arg(short = 's', long = "system")]
    pub system: Option<String>,

    /// Positional initial model name (e.g. `susi chat qwen2.5-coder:7b`)
    pub positional_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatSession {
    pub active_model: Option<String>,
    pub system_prompt: Option<String>,
    pub history: Vec<ChatMessage>,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub turn_count: usize,
}

impl ChatSession {
    pub fn new(model: Option<String>, system: Option<String>) -> Self {
        Self {
            active_model: model,
            system_prompt: system,
            history: Vec::new(),
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            turn_count: 0,
        }
    }
}

/// Formats the conversation history and the new user turn into a model prompt.
pub fn format_chat_prompt(
    system: Option<&str>,
    history: &[ChatMessage],
    new_user_input: &str,
) -> String {
    let mut out = String::new();
    if let Some(sys) = system.filter(|s| !s.trim().is_empty()) {
        out.push_str("System instructions:\n");
        out.push_str(sys.trim());
        out.push_str("\n\n");
    }
    for msg in history {
        match msg.role {
            ChatRole::User => {
                out.push_str("User: ");
                out.push_str(msg.content.trim());
                out.push_str("\n\n");
            }
            ChatRole::Assistant => {
                out.push_str("Assistant: ");
                out.push_str(msg.content.trim());
                out.push_str("\n\n");
            }
        }
    }
    out.push_str("User: ");
    out.push_str(new_user_input.trim());
    out.push_str("\n\nAssistant:");
    out
}

/// Slash command processing outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashCommandResult {
    Handled,
    Exit,
    NotACommand,
}

/// Handles `/command` inputs.
pub fn handle_slash_command(
    line: &str,
    session: &mut ChatSession,
    workspace: &Path,
) -> SlashCommandResult {
    let trimmed = line.trim();
    if !trimmed.starts_with('/') {
        return SlashCommandResult::NotACommand;
    }

    let mut parts = trimmed.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.collect::<Vec<_>>().join(" ");

    match cmd {
        "/help" | "/h" | "/?" => {
            println!("Commands:");
            println!("  /model [name]   View or change the active model (e.g. `/model qwen2.5-coder:7b` or `/model auto`)");
            println!(
                "  /models         List available local models and configured cloud providers"
            );
            println!("  /system [text]  View or update system prompt instructions");
            println!("  /tokens         Display session token counts and statistics");
            println!("  /clear          Clear conversation history");
            println!("  /exit, /quit    Exit the interactive chat session\n");
            SlashCommandResult::Handled
        }
        "/model" => {
            if arg.is_empty() {
                let current = session
                    .active_model
                    .as_deref()
                    .unwrap_or("auto (dynamic placement)");
                println!("Active model: {current}\n");
            } else if arg.eq_ignore_ascii_case("auto") {
                session.active_model = None;
                println!("Switched to dynamic auto routing.\n");
            } else {
                session.active_model = Some(arg.clone());
                println!("Switched model to `{arg}`.\n");
            }
            SlashCommandResult::Handled
        }
        "/models" => {
            println!("Available Local Models:");
            let local_models = ModelManager::list_models(workspace);
            if local_models.is_empty() {
                println!("  (no local GGUF models discovered under ~/.susi/models)");
            } else {
                for m in local_models {
                    println!("  - {} (provider: {})", m.name(), m.provider());
                }
            }
            println!("\nRegistered Providers & Backends:");
            let providers =
                susi_gemi::susi_core::registry::CapabilityRegistry::global().list_providers();
            if providers.is_empty() {
                println!("  (none registered)");
            } else {
                for p in providers {
                    println!("  - {p}");
                }
            }
            println!("\nUse `/model <name>` to switch.\n");
            SlashCommandResult::Handled
        }
        "/system" => {
            if arg.is_empty() {
                match &session.system_prompt {
                    Some(sys) => println!("Current system prompt:\n{sys}\n"),
                    None => {
                        println!("No system prompt set. Set one with `/system <instructions>`\n")
                    }
                }
            } else {
                session.system_prompt = Some(arg);
                println!("Updated system instructions.\n");
            }
            SlashCommandResult::Handled
        }
        "/tokens" => {
            println!("Session Statistics:");
            println!("  Turns completed:         {}", session.turn_count);
            println!("  Prompt tokens (approx):  {}", session.total_prompt_tokens);
            println!(
                "  Completion tokens (approx): {}",
                session.total_completion_tokens
            );
            println!(
                "  Total tokens:            {}\n",
                session.total_prompt_tokens + session.total_completion_tokens
            );
            SlashCommandResult::Handled
        }
        "/clear" => {
            session.history.clear();
            println!("Conversation history cleared.\n");
            SlashCommandResult::Handled
        }
        "/exit" | "/quit" | "/q" => SlashCommandResult::Exit,
        other => {
            println!("Unknown command `{other}`. Type `/help` for available commands.\n");
            SlashCommandResult::Handled
        }
    }
}

/// Execute `susi chat`.
pub fn execute(args: ChatArgs, workspace: &Path) -> anyhow::Result<()> {
    let initial_model = args.model.or(args.positional_model);
    let mut session = ChatSession::new(initial_model, args.system);

    let is_tty = io::stdin().is_terminal();

    if is_tty {
        let model_display = session
            .active_model
            .as_deref()
            .unwrap_or("auto (dynamic routing)");
        println!("╔══════════════════════════════════════════════════════════════════════╗");
        println!("║  SUSI Chat — Interactive Conversational REPL                         ║");
        println!("║  Active Model: {:<53} ║", model_display);
        println!("║  Type /help for slash commands · /exit or Ctrl-D to quit             ║");
        println!("╚══════════════════════════════════════════════════════════════════════╝\n");
    }

    loop {
        if is_tty {
            print!("susi> ");
            let _ = io::stdout().flush();
        }

        let mut line = String::new();
        let bytes_read = io::stdin().read_line(&mut line)?;
        if bytes_read == 0 {
            // EOF reached
            if is_tty {
                println!("\nGoodbye!");
            }
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match handle_slash_command(trimmed, &mut session, workspace) {
            SlashCommandResult::Handled => continue,
            SlashCommandResult::Exit => {
                if is_tty {
                    println!("Goodbye!");
                }
                break;
            }
            SlashCommandResult::NotACommand => {}
        }

        let full_prompt =
            format_chat_prompt(session.system_prompt.as_deref(), &session.history, trimmed);

        let serving_model = Arc::new(Mutex::new(
            session
                .active_model
                .clone()
                .unwrap_or_else(|| "auto".to_string()),
        ));
        let serving_model_cb = Arc::clone(&serving_model);

        let stdout_lock = Arc::new(Mutex::new(io::stdout()));
        let stdout_cb = Arc::clone(&stdout_lock);

        let start_time = Instant::now();

        let meta_callback = move |served: &str| {
            let mut m = serving_model_cb.lock().unwrap_or_else(|e| e.into_inner());
            *m = served.to_string();
        };

        let stream_callback = move |chunk: String| {
            let mut out = stdout_cb.lock().unwrap_or_else(|e| e.into_inner());
            let _ = out.write_all(chunk.as_bytes());
            let _ = out.flush();
        };

        let response = GemiEngine::generate_reasoning_stream_deep_meta(
            &full_prompt,
            workspace,
            &stream_callback,
            session.active_model.as_deref(),
            &meta_callback,
        );

        let elapsed = start_time.elapsed();
        let elapsed_secs = elapsed.as_secs_f64().max(0.001);
        let prompt_tokens = estimate_token_count(&full_prompt);
        let completion_tokens = estimate_token_count(&response);

        session.total_prompt_tokens += prompt_tokens;
        session.total_completion_tokens += completion_tokens;
        session.turn_count += 1;

        let tok_per_sec = (completion_tokens as f64) / elapsed_secs;
        let final_model = serving_model
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|_| "unknown".to_string());

        if !response.ends_with('\n') {
            println!();
        }

        if is_tty {
            eprintln!(
                "[susi] Served by {} · ~{} tokens in {:.2}s ({:.1} tok/s)\n",
                final_model, completion_tokens, elapsed_secs, tok_per_sec
            );
        }

        session.history.push(ChatMessage {
            role: ChatRole::User,
            content: trimmed.to_string(),
        });
        session.history.push(ChatMessage {
            role: ChatRole::Assistant,
            content: response,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_chat_prompt_single_turn() {
        let history = Vec::new();
        let prompt = format_chat_prompt(Some("Be concise"), &history, "What is Rust?");
        assert!(prompt.contains("System instructions:\nBe concise"));
        assert!(prompt.contains("User: What is Rust?"));
        assert!(prompt.ends_with("\n\nAssistant:"));
    }

    #[test]
    fn test_format_chat_prompt_multi_turn() {
        let history = vec![
            ChatMessage {
                role: ChatRole::User,
                content: "Hi".to_string(),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "Hello! How can I help?".to_string(),
            },
        ];
        let prompt = format_chat_prompt(None, &history, "What can you do?");
        assert!(prompt.contains("User: Hi\n\nAssistant: Hello! How can I help?"));
        assert!(prompt.contains("User: What can you do?"));
        assert!(prompt.ends_with("\n\nAssistant:"));
    }

    #[test]
    fn test_slash_commands() {
        let mut session = ChatSession::new(None, None);
        let ws = Path::new(".");

        assert_eq!(
            handle_slash_command("hello world", &mut session, ws),
            SlashCommandResult::NotACommand
        );

        assert_eq!(
            handle_slash_command("/help", &mut session, ws),
            SlashCommandResult::Handled
        );

        assert_eq!(
            handle_slash_command("/model qwen2.5-coder:7b", &mut session, ws),
            SlashCommandResult::Handled
        );
        assert_eq!(session.active_model.as_deref(), Some("qwen2.5-coder:7b"));

        assert_eq!(
            handle_slash_command("/model auto", &mut session, ws),
            SlashCommandResult::Handled
        );
        assert_eq!(session.active_model, None);

        assert_eq!(
            handle_slash_command("/system You are an expert", &mut session, ws),
            SlashCommandResult::Handled
        );
        assert_eq!(session.system_prompt.as_deref(), Some("You are an expert"));

        assert_eq!(
            handle_slash_command("/exit", &mut session, ws),
            SlashCommandResult::Exit
        );
    }
}
