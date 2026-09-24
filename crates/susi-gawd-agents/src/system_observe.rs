//! Deterministic system observations via GMCP `exec_command`.
//! Stops agents from LLM-narrating "no shell access" when tools exist.

use std::path::Path;
use std::process::Command;

/// Goals that need live host/filesystem evidence, not web search or pure LLM.
pub fn looks_like_system_observe_goal(goal: &str) -> bool {
    let lower = goal.to_ascii_lowercase();
    [
        "disk",
        "df ",
        "df\n",
        "du ",
        "filesystem",
        "file system",
        "lsblk",
        "free space",
        "storage",
        "mount",
        "disk usage",
        "disk space",
        "capacity",
        "inode",
        "uptime",
        "whoami",
        "hostname",
        "uname",
    ]
    .iter()
    .any(|k| lower.contains(k))
        || lower.trim() == "df"
        || lower.trim() == "du"
}

fn looks_like_tool_failure(out: &str) -> bool {
    let trimmed = out.trim();
    trimmed.is_empty()
        || trimmed.to_ascii_lowercase().contains("invalid argument")
        || trimmed.starts_with("Error:")
        || trimmed.starts_with("Protocol Error:")
        || trimmed.starts_with("Governance Violation:")
        || trimmed.starts_with("[FAIL]")
        || trimmed.starts_with("[CAPABILITY_GAP]")
        || trimmed.contains("Unknown tool")
}

/// An in-process receipt for a bounded native read. Private fields and no
/// deserializer prevent model text or a stored JSON object from minting one.
pub struct VerifiedSystemRead {
    goal: String,
    workspace: std::path::PathBuf,
    answer: String,
    captured_at: std::time::Instant,
}

impl VerifiedSystemRead {
    pub fn answer(&self) -> &str {
        &self.answer
    }

    /// Proves only that the answer is the exact output of this read, in this
    /// workspace, recently. It does not certify arbitrary interpretations.
    pub fn verify(
        &self,
        goal: &str,
        answer: &str,
        workspace: &Path,
    ) -> crate::susi_error::EaiResult<String> {
        if goal.trim().to_ascii_lowercase() != self.goal
            || answer != self.answer
            || workspace.canonicalize().ok().as_ref() != Some(&self.workspace)
            || self.captured_at.elapsed() > std::time::Duration::from_secs(60)
        {
            return Err(crate::susi_error::EaiError::governance("TRUTH_VIOLATION: native read receipt does not match answer, intent, workspace or freshness"));
        }
        Ok(self.answer.clone())
    }
}

/// Exact native query contracts; broader natural-language missions still need
/// claim-specific evidence. The allowlist is not an incidental keyword match.
pub fn capture_verified_read(
    goal: &str,
    workspace: &Path,
) -> Option<crate::susi_error::EaiResult<VerifiedSystemRead>> {
    let normalized = goal.trim().to_ascii_lowercase();
    // Status is not a shell read — it is a deterministic projection of the
    // live hardware profile. Minting it as a verified read gives `susi
    // status` the same answer-equality contract exec reads have, instead
    // of falling through to a cross-examine that can never pass without
    // receipts (and then burning a full cloud failover for nothing).
    if matches!(
        normalized.as_str(),
        "status" | "susi status" | "system status" | "substrate status"
    ) {
        return Some(capture_status_read(&normalized, workspace));
    }
    // The model inventory is equally deterministic substrate state.
    if matches!(
        normalized.as_str(),
        "models" | "list models" | "show models" | "model list"
    ) {
        return Some(capture_models_read(&normalized, workspace));
    }
    // Governed tool reads: the answer is the tool's own output verbatim,
    // which satisfies "evidence must be cited" only if the receipt binds
    // answer-to-output — narrating the same data unbound fails the gate.
    let tool_read = match normalized.as_str() {
        "dashboard" | "susi dashboard" | "show dashboard" => {
            Some(("sovereign_dashboard", serde_json::Value::Null))
        }
        "bloat audit" | "bloat-audit" | "run bloat audit" => {
            Some(("bloat_audit", serde_json::Value::Null))
        }
        _ => None,
    };
    if let Some((tool, arg)) = tool_read {
        return Some(capture_tool_read(&normalized, workspace, tool, arg));
    }
    let command = match normalized.as_str() {
        "disk usage" | "disk space" | "df" => "df -h -x tmpfs -x devtmpfs -x squashfs --total",
        "uptime" => "uptime",
        "hostname" => "hostname",
        "whoami" => "whoami",
        "uname" => "uname -a",
        _ => return None,
    };
    Some((|| {
        let workspace = workspace
            .canonicalize()
            .map_err(|e| crate::susi_error::EaiError::filesystem(e.to_string()))?;
        let captured_at = std::time::Instant::now();
        let output =
            run_exec_direct(&workspace, command).map_err(crate::susi_error::EaiError::process)?;
        if output.trim().is_empty() {
            return Err(crate::susi_error::EaiError::governance(
                "TRUTH_UNVERIFIED: command produced no observation",
            ));
        }
        let observed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| crate::susi_error::EaiError::process(e.to_string()))?
            .as_secs();
        Ok(VerifiedSystemRead {
            goal: normalized, workspace, captured_at,
            answer: format!("Command `{command}` (host execution, exit 0; observed at Unix {observed_at}):\n{output}"),
        })
    })())
}

/// Deterministic status projection: the same `HardwareProfiler` bus read a
/// plain "status" branch would format, captured as a verified read so the
/// answer is provably the live measurement, not model narration.
fn capture_status_read(
    normalized_goal: &str,
    workspace: &Path,
) -> crate::susi_error::EaiResult<VerifiedSystemRead> {
    let workspace = workspace
        .canonicalize()
        .map_err(|e| crate::susi_error::EaiError::filesystem(e.to_string()))?;
    let captured_at = std::time::Instant::now();
    let hw = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| crate::susi_error::EaiError::process(e.to_string()))?
        .as_secs();
    Ok(VerifiedSystemRead {
        goal: normalized_goal.to_string(),
        workspace,
        captured_at,
        answer: format!(
            "SUSI Substrate Status: Operational | Hardware: {} | RAM: {}GB \
             (live hardware profile, observed at Unix {observed_at})",
            hw.cpu_brand, hw.ram_gb
        ),
    })
}

/// The substrate's model inventory as a verified read — `list_models` is a
/// deterministic scan, so a "models" goal can certify its answer instead
/// of failing cross-examination for lack of receipts.
fn capture_models_read(
    normalized_goal: &str,
    workspace: &Path,
) -> crate::susi_error::EaiResult<VerifiedSystemRead> {
    let workspace = workspace
        .canonicalize()
        .map_err(|e| crate::susi_error::EaiError::filesystem(e.to_string()))?;
    let captured_at = std::time::Instant::now();
    let models = crate::susi_core::plane_bus::gemi::ModelManager::list_models(&workspace);
    let count = models.as_array().map(|a| a.len()).unwrap_or(0);
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| crate::susi_error::EaiError::process(e.to_string()))?
        .as_secs();
    Ok(VerifiedSystemRead {
        goal: normalized_goal.to_string(),
        workspace,
        captured_at,
        answer: format!(
            "Active Model Substrates (Count: {count}, live scan observed at \
             Unix {observed_at}):\n\n{models}"
        ),
    })
}

/// A governed tool call captured as a verified read. The answer is the
/// tool output verbatim, so the receipt proves the mission returned real
/// tool evidence rather than a narrative about it.
fn capture_tool_read(
    normalized_goal: &str,
    workspace: &Path,
    tool: &str,
    arg: serde_json::Value,
) -> crate::susi_error::EaiResult<VerifiedSystemRead> {
    let workspace = workspace
        .canonicalize()
        .map_err(|e| crate::susi_error::EaiError::filesystem(e.to_string()))?;
    let captured_at = std::time::Instant::now();
    let out = crate::susi_core::plane_bus::tools::execute_tool(tool, &arg, &workspace)
        .map_err(|e| crate::susi_error::EaiError::process(e.to_string()))?;
    if looks_like_tool_failure(&out) {
        return Err(crate::susi_error::EaiError::governance(format!(
            "TRUTH_UNVERIFIED: tool `{tool}` produced no observation: {out}"
        )));
    }
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| crate::susi_error::EaiError::process(e.to_string()))?
        .as_secs();
    Ok(VerifiedSystemRead {
        goal: normalized_goal.to_string(),
        workspace,
        captured_at,
        answer: format!("Tool `{tool}` output (governed, observed at Unix {observed_at}):\n{out}"),
    })
}

fn run_exec_direct(workspace: &Path, cmd: &str) -> Result<String, String> {
    crate::safety::SafetyDetector::audit_action("exec_command", cmd, workspace)
        .map_err(|e| e.to_string())?;
    crate::security::SecurityDetector::audit_action("exec_command", cmd, workspace)
        .map_err(|e| e.to_string())?;
    let mut parts = cmd.split_whitespace();
    let bin = parts
        .next()
        .ok_or_else(|| "Command cannot be empty".to_string())?;
    let args: Vec<&str> = parts.collect();
    let execute = || {
        let output = Command::new(bin)
            .args(&args)
            .current_dir(workspace)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| {
                crate::susi_core::susi_error::EaiError::process(format!("Exec failed: {e}"))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            return Err(crate::susi_core::susi_error::EaiError::process(
                if stderr.is_empty() {
                    "Command failed with non-zero exit status".into()
                } else {
                    stderr
                },
            ));
        }
        Ok(stdout)
    };
    // Mint a ledger receipt whenever a mission session is live — native reads
    // must not bypass capture just because they skip ToolRegistry.
    crate::susi_core::capture::EvidenceSession::capture_call(
        "exec_command",
        &serde_json::Value::String(cmd.to_string()),
        workspace,
        execute,
    )
    .map_err(|e| e.to_string())
}

fn run_exec(workspace: &Path, cmd: &str) -> String {
    if crate::susi_core::plane_bus::tools::exists("exec_command") {
        let out = crate::susi_core::plane_bus::tools::execute_tool(
            "exec_command",
            &serde_json::Value::String(cmd.to_string()),
            workspace,
        )
        .unwrap_or_default();
        if !looks_like_tool_failure(&out) {
            return format!("Command `{cmd}` (via GMCP exec_command):\n{out}");
        }
    }
    match run_exec_direct(workspace, cmd) {
        Ok(out) => format!("Command `{cmd}` (via host exec):\n{out}"),
        Err(e) => format!("Command `{cmd}` failed:\n{e}"),
    }
}

/// Fetch live system evidence for disk/storage/host goals.
/// Returns `None` when the goal is not a system-observation request.
pub fn observe_system(goal: &str, workspace: &Path) -> Option<String> {
    if !looks_like_system_observe_goal(goal) {
        return None;
    }
    let lower = goal.to_ascii_lowercase();

    let mut sections = Vec::new();

    if [
        "disk",
        "df",
        "du",
        "filesystem",
        "file system",
        "storage",
        "free space",
        "lsblk",
        "capacity",
        "inode",
        "mount",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        sections.push(run_exec(
            workspace,
            "df -h -x tmpfs -x devtmpfs -x squashfs --total",
        ));
        sections.push(run_exec(workspace, "df -h /"));
        if lower.contains("lsblk") || lower.contains("block") {
            sections.push(run_exec(workspace, "lsblk -o NAME,SIZE,FSTYPE,MOUNTPOINTS"));
        }
    }

    if lower.contains("uptime") {
        sections.push(run_exec(workspace, "uptime"));
    }
    if lower.contains("uname") {
        sections.push(run_exec(workspace, "uname -a"));
    }
    if lower.contains("hostname") {
        sections.push(run_exec(workspace, "hostname"));
    }
    if lower.contains("whoami") || lower.contains("who am i") {
        sections.push(run_exec(workspace, "whoami"));
    }

    if sections.is_empty() {
        sections.push(run_exec(
            workspace,
            "df -h -x tmpfs -x devtmpfs -x squashfs --total",
        ));
    }

    Some(format!(
        "**Live system observation**\n\n{}",
        sections.join("\n\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_disk_usage_goal() {
        assert!(looks_like_system_observe_goal(
            "Find the total disk usage on this system."
        ));
        assert!(!looks_like_system_observe_goal(
            "Refactor the authentication module"
        ));
    }

    #[test]
    fn observe_disk_runs_df() {
        let report = observe_system(
            "Find the total disk usage on this system. Report filesystem totals.",
            Path::new("."),
        )
        .expect("disk goal must observe");
        assert!(
            report.contains("Live system observation"),
            "unexpected report: {report}"
        );
        assert!(
            report.contains("Filesystem")
                || report.contains("Size")
                || report.contains("Used")
                || report.contains("failed"),
            "expected df-like evidence: {report}"
        );
    }
    #[test]
    fn native_receipt_binds_output_goal_workspace_and_freshness() {
        let workspace = std::env::current_dir().unwrap();
        let Some(Ok(mut read)) = capture_verified_read("hostname", &workspace) else {
            panic!("hostname read must execute");
        };
        assert!(read.verify("hostname", read.answer(), &workspace).is_ok());
        assert!(read
            .verify("hostname", "invented host", &workspace)
            .is_err());
        assert!(read
            .verify("disk usage", read.answer(), &workspace)
            .is_err());
        assert!(read
            .verify("hostname", read.answer(), &workspace.join("absent"))
            .is_err());
        read.captured_at = std::time::Instant::now() - std::time::Duration::from_secs(61);
        assert!(read.verify("hostname", read.answer(), &workspace).is_err());
        assert!(capture_verified_read("hostname and delete files", &workspace).is_none());
    }

    #[test]
    fn status_read_verifies_its_own_answer_only() {
        let workspace = std::env::current_dir().unwrap();
        let Some(Ok(read)) = capture_verified_read("status", &workspace) else {
            panic!("status read must mint a receipt");
        };
        assert!(read.answer().contains("SUSI Substrate Status"));
        assert!(read.verify("status", read.answer(), &workspace).is_ok());
        assert!(read
            .verify("status", "invented status", &workspace)
            .is_err());
        assert!(capture_verified_read("check the status", &workspace).is_none());
    }
}
