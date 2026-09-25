//! Auto-generated Self-Healing Runbooks (Swarm OS Bullet 16)
//!
//! When an agent fails, the Swarm OS auto-generates a Markdown runbook detailing
//! the failure state, linked logs, and recovery steps.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Details required to generate a post-mortem runbook.
#[derive(Debug, Clone)]
pub struct FailureContext {
    pub cell_id: String,
    pub failure_reason: String,
    pub stack_trace: Option<String>,
    pub related_logs: Vec<String>,
}

pub struct SelfHealingManager {
    runbooks_dir: PathBuf,
}

impl SelfHealingManager {
    pub fn new(workspace: &Path) -> std::io::Result<Self> {
        let runbooks_dir = workspace.join("runbooks");
        if !runbooks_dir.exists() {
            fs::create_dir_all(&runbooks_dir)?;
        }
        Ok(Self { runbooks_dir })
    }

    /// Generates a Markdown runbook for a failed cell.
    pub fn generate_runbook(&self, ctx: FailureContext) -> std::io::Result<PathBuf> {
        let ts = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let runbook_path = self
            .runbooks_dir
            .join(format!("incident_{}_{}.md", ctx.cell_id, ts));

        let mut content = format!("# Incident Report: {}\n\n", ctx.cell_id);
        content.push_str(&format!("**Time:** {}\n", ts));
        content.push_str(&format!("**Failure Reason:** {}\n\n", ctx.failure_reason));

        content.push_str("## Recovery Steps\n");
        content.push_str("1. Review the failure reason above.\n");
        content.push_str("2. Check the linked logs for contextual warnings.\n");
        content.push_str("3. If this is a transient error, the Scheduler will automatically retry the task on another cell (Swarm OS Bullet 8 / Durable Workflows).\n");
        content.push_str("4. If the cell binary is corrupt, rebuild and place it in `~/.susi/cells/` to trigger Hot-Plug discovery (Swarm OS Bullet 6).\n\n");

        if let Some(stack) = ctx.stack_trace {
            content.push_str("## Stack Trace\n```rust\n");
            content.push_str(&stack);
            content.push_str("\n```\n\n");
        }

        if !ctx.related_logs.is_empty() {
            content.push_str("## Related Logs\n");
            for log in ctx.related_logs {
                content.push_str(&format!("- `{}`\n", log));
            }
        }

        fs::write(&runbook_path, content)?;
        Ok(runbook_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_generate_runbook() {
        let temp_dir = env::temp_dir().join(format!("susi_runbook_test_{}", std::process::id()));
        let manager = SelfHealingManager::new(&temp_dir).unwrap();

        let ctx = FailureContext {
            cell_id: "test-cell-99".to_string(),
            failure_reason: "OOM Killed".to_string(),
            stack_trace: None,
            related_logs: vec!["/var/log/syslog".to_string()],
        };

        let path = manager.generate_runbook(ctx).unwrap();
        assert!(path.exists());

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Incident Report: test-cell-99"));
        assert!(content.contains("OOM Killed"));

        let _ = fs::remove_dir_all(temp_dir);
    }
}
