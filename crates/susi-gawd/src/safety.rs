// Blocks destructive commands and writes to critical system paths, using
// patterns loaded from config plus a hardcoded exec_command allowlist.

use std::path::Path;
use susi_error::{EaiError, EaiResult};
use susi_sandbox::manager::SusiConfig;

pub struct SafetyDetector;

impl SafetyDetector {
    pub fn audit_action(tool_name: &str, arg: &str, _workspace: &Path) -> EaiResult<()> {
        let global_dir = susi_paths::SusiDirs::config_dir();
        let cfg = SusiConfig::load(&global_dir).unwrap_or_default();
        let patterns = cfg.governance();

        let lower_arg = arg.to_lowercase();

        // C4 Security Patch: Command Allowlist
        if tool_name == "exec_command" {
            let allowed_bins = [
                "cargo", "git", "rustc", "susi", "sed", "grep", "rg", "cat", "ls", "find", "fd",
                "echo", "pwd", "df", "du", "lsblk", "free", "uptime", "uname", "hostname",
                "whoami", "head", "tail", "wc", "stat", "file", "which", "env", "id",
            ];
            let cmd_bin = lower_arg.split_whitespace().next().unwrap_or("");
            if !allowed_bins.contains(&cmd_bin) {
                return Err(EaiError::governance(format!(
                    "C4 BLOCK: Executable '{}' not in strict allowlist",
                    cmd_bin
                )));
            }
        }

        // 1. Command Pattern Check (Dynamic)
        for pattern in &patterns.destructive_commands {
            if lower_arg.contains(&pattern.to_lowercase()) {
                return Err(EaiError::governance(format!(
                    "Action contains restricted pattern '{}'",
                    pattern
                )));
            }
        }

        // 2. Critical Path Check (Dynamic)
        if tool_name == "write_file" || tool_name == "exec_command" || tool_name == "SUSI_SOLVE" {
            for path in &patterns.critical_system_paths {
                if lower_arg.contains(&path.to_lowercase()) {
                    return Err(EaiError::governance(format!(
                        "Action targets critical system path '{}'",
                        path
                    )));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_audit_safe_commands() {
        let ws = Path::new(".");
        assert!(SafetyDetector::audit_action("exec_command", "cargo check", ws).is_ok());
        assert!(SafetyDetector::audit_action("exec_command", "df -h /", ws).is_ok());
        assert!(SafetyDetector::audit_action("exec_command", "du -sh .", ws).is_ok());
    }

    #[test]
    fn test_safety_audit_destructive_patterns() {
        let ws = Path::new(".");
        // Note: These tests depend on the default config being loaded or present in ~/.susi/config.json
        // In a CI/test environment, we might need a controlled global_dir.
        assert!(SafetyDetector::audit_action("exec_command", "rm -rf /", ws).is_err());
    }
}
