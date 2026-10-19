//! High-speed coding model CLI (`susi code`).
//!
//! Generates, previews, and applies unified diffs directly to the workspace,
//! optimized for fast local coder models (e.g. `qwen2.5-coder:7b`) or cloud models.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use susi_gemi::engines::runtime::GemiEngine;

/// Arguments for `susi code`.
#[derive(Args, Debug, Clone)]
pub struct CodeArgs {
    /// Coding model name (e.g. `qwen2.5-coder:7b`, `claude-3-7-sonnet`, `deepseek-chat`)
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Target file(s) to inspect / supply as context / modify
    #[arg(short = 'f', long = "file")]
    pub files: Vec<PathBuf>,

    /// Print generated diff only; do not modify files on disk
    #[arg(short = 'd', long = "diff-only")]
    pub diff_only: bool,

    /// Apply diff directly without interactive confirmation prompt
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Syntax check or test command to run after applying (e.g. `cargo check`, `npm test`)
    #[arg(long = "check")]
    pub check: Option<String>,

    /// Rollback changes if check command fails
    #[arg(long = "rollback-on-failure")]
    pub rollback: bool,

    /// Output JSON summary of the generated changes and execution
    #[arg(long = "json")]
    pub json: bool,

    /// Positional model name and/or coding instructions
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub instruction: Vec<String>,
}

/// JSON output format for `susi code --json`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CodeJsonOutput {
    pub model: String,
    pub files_modified: Vec<String>,
    pub diff: String,
    pub applied: bool,
    pub check_passed: Option<bool>,
    pub tokens: usize,
    pub duration_ms: u128,
    pub tok_per_sec: f64,
}

/// A parsed unified diff hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<String>,
}

/// A patch targeting a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<DiffHunk>,
    pub direct_content: Option<String>,
}

/// Strip standard prefixes like `a/`, `b/`, leading slashes or quotes from diff paths.
pub fn clean_diff_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
    let path = if let Some(stripped) = trimmed.strip_prefix("a/") {
        stripped
    } else if let Some(stripped) = trimmed.strip_prefix("b/") {
        stripped
    } else {
        trimmed
    };
    path.trim_start_matches('/').to_string()
}

/// Check that a relative target path stays confined strictly within the workspace.
pub fn confined_path(workspace: &Path, rel: &str) -> Result<PathBuf, String> {
    let clean = clean_diff_path(rel);
    if clean.is_empty() || clean == "/dev/null" {
        return Ok(PathBuf::from("/dev/null"));
    }
    let target = workspace.join(&clean);
    // Canonicalize workspace
    let canonical_ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    // Normalize target path without escaping
    let mut normalized = PathBuf::new();
    for component in target.components() {
        match component {
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!("path escapes workspace: {rel}"));
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component),
        }
    }

    if !normalized.starts_with(&canonical_ws) && !normalized.starts_with(workspace) {
        return Err(format!("path escapes workspace: {rel}"));
    }

    Ok(normalized)
}

/// Parse unified diff hunks from a text block.
pub fn parse_unified_diff(text: &str) -> Vec<FilePatch> {
    let mut patches = Vec::new();
    let mut current_patch: Option<FilePatch> = None;
    let mut current_hunk: Option<DiffHunk> = None;

    // Helper to finish current hunk
    fn finish_hunk(patch: &mut FilePatch, hunk: &mut Option<DiffHunk>) {
        if let Some(h) = hunk.take() {
            if !h.lines.is_empty() {
                patch.hunks.push(h);
            }
        }
    }

    // Helper to finish current patch
    fn finish_patch(
        patches: &mut Vec<FilePatch>,
        patch: &mut Option<FilePatch>,
        hunk: &mut Option<DiffHunk>,
    ) {
        if let Some(mut p) = patch.take() {
            finish_hunk(&mut p, hunk);
            if !p.hunks.is_empty() || p.direct_content.is_some() {
                patches.push(p);
            }
        }
    }

    // First scan for code blocks like ```diff ... ``` or ```patch ... ```
    let lines: Vec<&str> = text.lines().collect();
    let mut in_diff_block = false;
    let mut filtered_lines: Vec<&str> = Vec::new();

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("```diff") || trimmed.starts_with("```patch") {
            in_diff_block = true;
            continue;
        } else if in_diff_block && trimmed.starts_with("```") {
            in_diff_block = false;
            continue;
        }

        if in_diff_block {
            filtered_lines.push(line);
        }
    }

    // If no explicit ```diff block found, try using the entire text
    let target_lines = if filtered_lines.is_empty() {
        &lines[..]
    } else {
        &filtered_lines[..]
    };

    let mut i = 0;
    while i < target_lines.len() {
        let line = target_lines[i];

        if let Some(rest) = line.strip_prefix("--- ") {
            finish_patch(&mut patches, &mut current_patch, &mut current_hunk);
            let old_raw = rest.split_whitespace().next().unwrap_or("");
            let old_path = clean_diff_path(old_raw);

            // Expect next line to be +++
            let mut new_path = old_path.clone();
            if i + 1 < target_lines.len() {
                if let Some(new_rest) = target_lines[i + 1].strip_prefix("+++ ") {
                    i += 1;
                    let new_raw = new_rest.split_whitespace().next().unwrap_or("");
                    new_path = clean_diff_path(new_raw);
                }
            }

            current_patch = Some(FilePatch {
                old_path,
                new_path,
                hunks: Vec::new(),
                direct_content: None,
            });
            i += 1;
            continue;
        }

        if line.starts_with("@@ ") {
            if let Some(ref mut patch) = current_patch {
                finish_hunk(patch, &mut current_hunk);
                if let Some(hunk) = parse_hunk_header(line) {
                    current_hunk = Some(hunk);
                }
            }
            i += 1;
            continue;
        }

        if let Some(ref mut hunk) = current_hunk {
            if line.starts_with('+')
                || line.starts_with('-')
                || line.starts_with(' ')
                || line.starts_with('\\')
            {
                hunk.lines.push(line.to_string());
            } else if !line.trim().is_empty() && !line.starts_with("diff ") {
                // If the model omitted leading space for context lines, preserve it
                let mut adjusted = String::with_capacity(line.len() + 1);
                adjusted.push(' ');
                adjusted.push_str(line);
                hunk.lines.push(adjusted);
            }
        }

        i += 1;
    }

    finish_patch(&mut patches, &mut current_patch, &mut current_hunk);

    // If no unified diff was found, check for file replacement codeblocks:
    // e.g. ```rust:path/to/file.rs or ```path/to/file.rs
    if patches.is_empty() {
        patches = parse_file_replacement_blocks(text);
    }

    patches
}

/// Parse hunk header e.g. `@@ -10,4 +10,5 @@` or `@@ -1 +1 @@`
fn parse_hunk_header(header: &str) -> Option<DiffHunk> {
    let parts: Vec<&str> = header.split("@@").collect();
    if parts.len() < 2 {
        return None;
    }
    let range_part = parts[1].trim();
    let ranges: Vec<&str> = range_part.split_whitespace().collect();
    if ranges.len() < 2 {
        return None;
    }

    let parse_range = |s: &str| -> (usize, usize) {
        let clean = s.trim_start_matches('-').trim_start_matches('+');
        let parts: Vec<&str> = clean.split(',').collect();
        let start = parts.first().and_then(|p| p.parse().ok()).unwrap_or(1);
        let len = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(1);
        (start, len)
    };

    let (old_start, old_len) = parse_range(ranges[0]);
    let (new_start, new_len) = parse_range(ranges[1]);

    Some(DiffHunk {
        old_start,
        old_len,
        new_start,
        new_len,
        lines: Vec::new(),
    })
}

/// Fallback parser for code blocks formatted as ```` ```lang:path/to/file ```` or ```` ```path/to/file ````.
fn parse_file_replacement_blocks(text: &str) -> Vec<FilePatch> {
    let mut patches = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(header) = line.strip_prefix("```") {
            let header = header.trim();
            let file_path = if let Some((_lang, path)) = header.split_once(':') {
                Some(path.trim())
            } else if header.contains('/') || header.contains('.') {
                Some(header)
            } else {
                None
            };

            if let Some(path) = file_path {
                let clean_path = clean_diff_path(path);
                if !clean_path.is_empty() {
                    let mut content = Vec::new();
                    i += 1;
                    while i < lines.len() && !lines[i].trim().starts_with("```") {
                        content.push(lines[i]);
                        i += 1;
                    }

                    patches.push(FilePatch {
                        old_path: clean_path.clone(),
                        new_path: clean_path,
                        hunks: Vec::new(),
                        direct_content: Some(content.join("\n")),
                    });
                }
            }
        }
        i += 1;
    }

    patches
}

/// Apply a single FilePatch to the existing file content.
pub fn apply_file_patch(existing_content: &str, patch: &FilePatch) -> Result<String, String> {
    if let Some(ref direct) = patch.direct_content {
        return Ok(direct.clone());
    }

    if patch.hunks.is_empty() {
        return Ok(existing_content.to_string());
    }

    let existing_lines: Vec<&str> = if existing_content.is_empty() {
        Vec::new()
    } else {
        existing_content.lines().collect()
    };

    let mut current_lines = existing_lines;

    for hunk in &patch.hunks {
        let mut expected_old = Vec::new();
        let mut replacement = Vec::new();

        for hline in &hunk.lines {
            if hline.starts_with('\\') {
                continue;
            }
            let tag = hline.chars().next().unwrap_or(' ');
            let rest = &hline[1..];
            match tag {
                ' ' => {
                    expected_old.push(rest);
                    replacement.push(rest);
                }
                '-' => {
                    expected_old.push(rest);
                }
                '+' => {
                    replacement.push(rest);
                }
                _ => {
                    expected_old.push(rest);
                    replacement.push(rest);
                }
            }
        }

        // If newly created file
        if current_lines.is_empty() && expected_old.is_empty() {
            current_lines = replacement;
            continue;
        }

        // Locate match position for expected_old
        let match_idx = if expected_old.is_empty() {
            Some(hunk.old_start.saturating_sub(1).min(current_lines.len()))
        } else {
            find_hunk_match(&current_lines, &expected_old, hunk.old_start)
        };

        match match_idx {
            Some(idx) => {
                let end_idx = idx + expected_old.len();
                let mut new_lines = Vec::with_capacity(
                    current_lines.len() + replacement.len() - expected_old.len(),
                );
                new_lines.extend_from_slice(&current_lines[..idx]);
                new_lines.extend_from_slice(&replacement);
                if end_idx <= current_lines.len() {
                    new_lines.extend_from_slice(&current_lines[end_idx..]);
                }
                current_lines = new_lines;
            }
            None => {
                return Err(format!(
                    "Failed to find context match for hunk @@ -{},{} in {}",
                    hunk.old_start, hunk.old_len, patch.new_path
                ));
            }
        }
    }

    let mut result = current_lines.join("\n");
    if existing_content.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

/// Find index where expected lines match in existing lines, with tolerance for line drift and whitespace.
fn find_hunk_match(existing: &[&str], expected: &[&str], suggested_start: usize) -> Option<usize> {
    if expected.is_empty() {
        return Some(suggested_start.saturating_sub(1).min(existing.len()));
    }
    let exp_len = expected.len();
    if exp_len > existing.len() {
        return None;
    }

    // 1. Try suggested position
    let suggested_idx = suggested_start.saturating_sub(1);
    if suggested_idx + exp_len <= existing.len()
        && &existing[suggested_idx..suggested_idx + exp_len] == expected
    {
        return Some(suggested_idx);
    }

    // 2. Scan exact match throughout the file
    for i in 0..=(existing.len() - exp_len) {
        if &existing[i..i + exp_len] == expected {
            return Some(i);
        }
    }

    // 3. Scan trimmed match (ignoring leading/trailing whitespace differences)
    for i in 0..=(existing.len() - exp_len) {
        let matches_trimmed = (0..exp_len).all(|j| existing[i + j].trim() == expected[j].trim());
        if matches_trimmed {
            return Some(i);
        }
    }

    None
}

/// Colorize diff lines for terminal output.
pub fn colorize_diff(diff: &str) -> String {
    let mut out = String::new();
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            out.push_str("\x1b[1m");
            out.push_str(line);
            out.push_str("\x1b[0m\n");
        } else if line.starts_with('+') {
            out.push_str("\x1b[32m");
            out.push_str(line);
            out.push_str("\x1b[0m\n");
        } else if line.starts_with('-') {
            out.push_str("\x1b[31m");
            out.push_str(line);
            out.push_str("\x1b[0m\n");
        } else if line.starts_with("@@") {
            out.push_str("\x1b[36m");
            out.push_str(line);
            out.push_str("\x1b[0m\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Prompt user for confirmation before applying diffs.
fn confirm_apply() -> bool {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return false;
    }
    print!("Apply these changes to workspace? [y/N]: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let trimmed = input.trim().to_ascii_lowercase();
        trimmed == "y" || trimmed == "yes"
    } else {
        false
    }
}

/// Read bounded stdin input if available.
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

/// Resolves model and instructions from CLI args and stdin.
pub fn resolve_model_and_instruction(
    cli_model: Option<String>,
    tokens: &[String],
    stdin_input: Option<String>,
) -> anyhow::Result<(Option<String>, String)> {
    let mut model = cli_model;
    let user_text = if model.is_some() {
        tokens.join(" ")
    } else if tokens.is_empty() {
        String::new()
    } else if tokens.len() == 1 {
        let first = &tokens[0];
        if stdin_input.is_some() && super::run_cli::looks_like_model(first) {
            model = Some(first.clone());
            String::new()
        } else {
            first.clone()
        }
    } else {
        let first = &tokens[0];
        if super::run_cli::looks_like_model(first) {
            model = Some(first.clone());
            tokens[1..].join(" ")
        } else {
            tokens.join(" ")
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
        anyhow::bail!(
            "No instruction provided. Usage: susi code [model] [-f file] \"<instruction>\""
        );
    }

    Ok((model, trimmed))
}

/// Assemble full coding prompt including target file contexts.
pub fn build_coding_prompt(
    workspace: &Path,
    target_files: &[PathBuf],
    instruction: &str,
) -> anyhow::Result<String> {
    let mut prompt = String::new();
    prompt.push_str("System instructions:\n");
    prompt.push_str("You are SUSI Code, a high-speed precision coding model.\n");
    prompt.push_str(
        "When modifying code, output standard unified diffs inside ```diff ... ``` blocks:\n",
    );
    prompt.push_str("```diff\n--- a/path/to/file\n+++ b/path/to/file\n@@ -start,len +start,len @@\n context\n-removed\n+added\n context\n```\n");
    prompt.push_str("For newly created or completely rewritten files, use standard diff with /dev/null or ```lang:path/to/file```.\n\n");

    if !target_files.is_empty() {
        prompt.push_str("File Context:\n");
        for file in target_files {
            let rel_path = file.to_string_lossy().to_string();
            let full_path =
                confined_path(workspace, &rel_path).map_err(|e| anyhow::anyhow!("{e}"))?;

            if full_path.is_file() {
                let content = fs::read_to_string(&full_path).map_err(|e| {
                    anyhow::anyhow!("failed to read file {}: {e}", full_path.display())
                })?;
                prompt.push_str(&format!(
                    "--- File: {} ---\n```\n{}\n```\n\n",
                    rel_path, content
                ));
            } else {
                prompt.push_str(&format!("--- File: {} (new/empty) ---\n\n", rel_path));
            }
        }
    }

    prompt.push_str("User Request:\n");
    prompt.push_str(instruction);

    Ok(prompt)
}

/// Execute `susi code`.
pub fn execute(args: CodeArgs, workspace: &Path) -> anyhow::Result<()> {
    let stdin_input = read_stdin_bounded()?;
    let (model, instruction) =
        resolve_model_and_instruction(args.model, &args.instruction, stdin_input)?;

    let full_prompt = build_coding_prompt(workspace, &args.files, &instruction)?;

    let serving_model = Arc::new(Mutex::new(
        model.clone().unwrap_or_else(|| "auto".to_string()),
    ));
    let serving_model_cb = Arc::clone(&serving_model);

    let stdout_lock = Arc::new(Mutex::new(io::stdout()));
    let stdout_cb = Arc::clone(&stdout_lock);

    let stream = !args.diff_only && !args.json;
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

    let elapsed = start_time.elapsed();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let final_model = serving_model
        .lock()
        .map(|m| m.clone())
        .unwrap_or_else(|_| "unknown".to_string());

    let tokens = super::run_cli::estimate_token_count(&response);
    let tok_per_sec = (tokens as f64) / elapsed_secs;

    let patches = parse_unified_diff(&response);

    if patches.is_empty() {
        if !stream && !args.json {
            println!("{response}");
        }
        if args.json {
            let output = CodeJsonOutput {
                model: final_model,
                files_modified: Vec::new(),
                diff: String::new(),
                applied: false,
                check_passed: None,
                tokens,
                duration_ms: elapsed.as_millis(),
                tok_per_sec,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        return Ok(());
    }

    // Build diff representation
    let mut diff_repr = String::new();
    for p in &patches {
        if let Some(ref direct) = p.direct_content {
            diff_repr.push_str(&format!(
                "+++ {}\n(full replacement {} bytes)\n\n",
                p.new_path,
                direct.len()
            ));
        } else {
            diff_repr.push_str(&format!("--- a/{}\n+++ b/{}\n", p.old_path, p.new_path));
            for h in &p.hunks {
                diff_repr.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    h.old_start, h.old_len, h.new_start, h.new_len
                ));
                for l in &h.lines {
                    diff_repr.push_str(l);
                    diff_repr.push('\n');
                }
            }
        }
    }

    if args.diff_only {
        if args.json {
            let output = CodeJsonOutput {
                model: final_model,
                files_modified: patches.iter().map(|p| p.new_path.clone()).collect(),
                diff: diff_repr,
                applied: false,
                check_passed: None,
                tokens,
                duration_ms: elapsed.as_millis(),
                tok_per_sec,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("\n{}", colorize_diff(&diff_repr));
        }
        return Ok(());
    }

    // Confirm application if not --yes
    let should_apply = args.yes || confirm_apply();

    if !should_apply {
        println!("Changes not applied.");
        return Ok(());
    }

    // Prepare backups for rollback
    let mut backups: Vec<(PathBuf, Option<String>)> = Vec::new();
    let mut applied_files = Vec::new();

    for p in &patches {
        let full_path =
            confined_path(workspace, &p.new_path).map_err(|e| anyhow::anyhow!("{e}"))?;

        if full_path.to_string_lossy() == "/dev/null" {
            continue;
        }

        let existing = if full_path.is_file() {
            Some(fs::read_to_string(&full_path)?)
        } else {
            None
        };

        let new_content = apply_file_patch(existing.as_deref().unwrap_or(""), p)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        backups.push((full_path.clone(), existing));

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full_path, &new_content)?;
        applied_files.push(p.new_path.clone());
    }

    // Run check command if specified
    let mut check_passed = None;
    if let Some(ref check_cmd) = args.check {
        let status = Command::new(susi_core::bounded_cmd::posix_shell())
            .arg("-c")
            .arg(check_cmd)
            .current_dir(workspace)
            .status();

        let passed = status.map(|s| s.success()).unwrap_or(false);
        check_passed = Some(passed);

        if !passed {
            if args.rollback {
                for (path, orig) in backups {
                    if let Some(prev) = orig {
                        let _ = fs::write(path, prev);
                    } else {
                        let _ = fs::remove_file(path);
                    }
                }
                anyhow::bail!(
                    "Check command failed: '{}' (all changes rolled back)",
                    check_cmd
                );
            } else {
                eprintln!("⚠️ Warning: Check command failed: '{}'", check_cmd);
            }
        } else {
            println!("✅ Check command passed: '{}'", check_cmd);
        }
    }

    if args.json {
        let output = CodeJsonOutput {
            model: final_model,
            files_modified: applied_files,
            diff: diff_repr,
            applied: true,
            check_passed,
            tokens,
            duration_ms: elapsed.as_millis(),
            tok_per_sec,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "\n✅ Successfully applied changes to {} file(s).",
            applied_files.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_diff_path() {
        assert_eq!(clean_diff_path("a/src/main.rs"), "src/main.rs");
        assert_eq!(
            clean_diff_path("b/src/cli/run_cli.rs"),
            "src/cli/run_cli.rs"
        );
        assert_eq!(clean_diff_path("\"a/foo.rs\""), "foo.rs");
        assert_eq!(clean_diff_path("/dev/null"), "dev/null");
    }

    #[test]
    fn test_confined_path() {
        let ws = std::env::temp_dir();
        let valid = confined_path(&ws, "src/foo.rs").unwrap();
        assert!(valid.starts_with(&ws));

        let invalid = confined_path(&ws, "../../etc/passwd");
        assert!(invalid.is_err());
    }

    #[test]
    fn test_parse_unified_diff() {
        let diff_text = r#"
Here is the fix:
```diff
--- a/src/calc.rs
+++ b/src/calc.rs
@@ -1,3 +1,4 @@
 fn add(a: i32, b: i32) -> i32 {
+    // add two numbers
     a + b
 }
```
"#;
        let patches = parse_unified_diff(diff_text);
        assert_eq!(patches.len(), 1);
        let p = &patches[0];
        assert_eq!(p.old_path, "src/calc.rs");
        assert_eq!(p.new_path, "src/calc.rs");
        assert_eq!(p.hunks.len(), 1);
        let h = &p.hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.new_start, 1);
    }

    #[test]
    fn test_apply_file_patch_modification() {
        let original = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let diff = r#"```diff
--- a/src/calc.rs
+++ b/src/calc.rs
@@ -1,3 +1,4 @@
 fn add(a: i32, b: i32) -> i32 {
+    let sum = a + b;
-    a + b
+    sum
 }
```"#;
        let patches = parse_unified_diff(diff);
        assert_eq!(patches.len(), 1);
        let result = apply_file_patch(original, &patches[0]).unwrap();
        assert!(result.contains("let sum = a + b;"));
        assert!(result.contains("sum"));
        assert!(!result.contains("a + b\n}"));
    }

    #[test]
    fn test_apply_file_patch_new_file() {
        let original = "";
        let diff = r#"```diff
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,3 @@
+pub fn hello() -> &'static str {
+    "hello"
+}
```"#;
        let patches = parse_unified_diff(diff);
        assert_eq!(patches.len(), 1);
        let result = apply_file_patch(original, &patches[0]).unwrap();
        assert!(result.contains("pub fn hello()"));
    }

    #[test]
    fn test_colorize_diff() {
        let diff = "--- a/foo\n+++ b/foo\n@@ -1 +1 @@\n+added\n-removed\n context";
        let colored = colorize_diff(diff);
        assert!(colored.contains("\x1b[32m+added"));
        assert!(colored.contains("\x1b[31m-removed"));
        assert!(colored.contains("\x1b[36m@@"));
    }
}
