//! Susi Native Board CLI (`susi board ...`) and Core git-hook guard.

use crate::anyhow::{anyhow, Result};
use std::path::Path;
pub use susi_cli_defs::board::BoardCommands;
use susi_core::native_board::{
    check_board, format_full_item, list_items, post_item, resolve_agent, resolve_board_target,
    resolve_host, summary_line, BoardDraft,
};
use susi_core::worktree_isolation::require_isolated_worktree;

pub fn execute(action: BoardCommands, workspace: &Path) -> Result<()> {
    match action {
        BoardCommands::List { limit, kind, json } => {
            let target = resolve_board_target(Some(workspace)).map_err(|e| anyhow!(e))?;
            let (mut items, findings) = list_items(&target).map_err(|e| anyhow!(e))?;
            if let Some(kind) = kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                items.retain(|s| s.item.kind == kind);
            }
            if let Some(lim) = limit {
                if items.len() > lim {
                    items = items.split_off(items.len() - lim);
                }
            }
            if json {
                let doc = serde_json::json!({ "items": items, "findings": findings });
                println!("{}", serde_json::to_string_pretty(&doc)?);
            } else {
                for f in &findings {
                    eprintln!("! {f}");
                }
                for item in &items {
                    println!("{}", summary_line(item));
                }
            }
            if !findings.is_empty() {
                return Err(anyhow!("board integrity findings present"));
            }
        }
        BoardCommands::Show { n, json } => {
            let target = resolve_board_target(Some(workspace)).map_err(|e| anyhow!(e))?;
            let (items, findings) = list_items(&target).map_err(|e| anyhow!(e))?;
            let stored = items
                .into_iter()
                .find(|s| s.n == n)
                .ok_or_else(|| anyhow!("board item {n} does not exist"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stored)?);
            } else {
                for f in &findings {
                    eprintln!("! {f}");
                }
                print!("{}", format_full_item(&stored));
            }
        }
        BoardCommands::Check {
            agent,
            allow_empty,
            json,
        } => {
            let agent = resolve_agent(agent.as_deref(), Some(workspace)).map_err(|e| anyhow!(e))?;
            let target = resolve_board_target(Some(workspace)).map_err(|e| anyhow!(e))?;
            let status = check_board(&target, &agent, allow_empty).map_err(|e| anyhow!(e))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "Susi Native Board OK for `{agent}` ({} items, last #{})",
                    status.items, status.last
                );
            }
        }
        BoardCommands::Post {
            kind,
            title,
            body,
            agent,
            to,
            re,
            task,
            start,
            json,
        } => {
            let agent = resolve_agent(agent.as_deref(), Some(workspace)).map_err(|e| anyhow!(e))?;
            let draft = BoardDraft {
                kind,
                agent,
                host: resolve_host(None),
                title,
                body,
                to,
                re,
                task,
            };
            let target = resolve_board_target(Some(workspace)).map_err(|e| anyhow!(e))?;
            let stored = post_item(&target, &draft, start).map_err(|e| anyhow!(e))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stored)?);
            } else {
                println!("Posted #{} ({})", stored.n, stored.sha);
            }
        }
        BoardCommands::Guard { stage, agent } => {
            let stage_norm = stage.trim().to_ascii_lowercase();
            let resolved_agent =
                resolve_agent(agent.as_deref(), Some(workspace)).map_err(|e| anyhow!(e))?;
            match stage_norm.as_str() {
                "commit" | "merge" | "push" => {
                    require_isolated_worktree(workspace, Some(&resolved_agent))
                        .map_err(|e| anyhow!(e))?;
                    let target = resolve_board_target(Some(workspace)).map_err(|e| anyhow!(e))?;
                    check_board(&target, &resolved_agent, false).map_err(|e| anyhow!(e))?;
                }
                other => {
                    return Err(anyhow!(
                        "unknown guard stage `{other}` (expected commit, merge, or push)"
                    ));
                }
            }
        }
    }
    Ok(())
}
