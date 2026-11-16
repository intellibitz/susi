//! Public storage audit contracts delegated through the authorized MCP socket.
use crate::git_governor::{caller_wire_path, workflow_call};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use susi_error::EaiError;

type Result<T> = std::result::Result<T, String>;

/// Active authorization input for explicitly directed remote-ref deletions.
pub const DIRECTED: &str = ".agents/storage/directed-deletions.txt";

/// What one run observed. `findings` empty means nothing was lost.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Protected refs whose remote value equals the record.
    pub unchanged: usize,
    /// Protected refs seen for the first time and recorded.
    pub recorded: usize,
    /// Append-only refs that moved forward; their record advanced.
    pub advanced: usize,
    /// Directed deletions accepted in this run; their retained copy is dropped.
    pub accepted: usize,
    /// One line per rewound, moved, deleted or re-created ref, with what to do.
    pub findings: Vec<String>,
}

impl Report {
    /// One line of counts for `remote`.
    pub fn summary(&self, remote: &str) -> String {
        let accepted = match self.accepted {
            0 => String::new(),
            count => format!(", {count} directed deletion(s) accepted"),
        };
        format!(
            "storage guard ({remote}): {} unchanged, {} moved forward, {} newly recorded, {} finding(s){accepted}",
            self.unchanged,
            self.advanced,
            self.recorded,
            self.findings.len()
        )
    }
}

/// Audit protected remote refs through the installed workflow provider.
pub fn audit(root: &Path, remote: &str) -> Result<Report> {
    let repo = caller_wire_path(root).map_err(|error| error.to_string())?;
    workflow_call("storage_audit", &json!({"repo":repo,"remote":remote})).map_err(|error| {
        if let EaiError::Process(message, _) = error {
            message
        } else {
            error.to_string()
        }
    })
}

/// Advisory witness result; an unavailable provider remains a visible warning.
pub fn witness(root: &Path, remote: &str) -> Option<String> {
    if std::env::var_os("SUSI_NO_STORAGE_GUARD").is_some_and(|value| value == "1") {
        return None;
    }
    match audit(root, remote) {
        Ok(report) if report.findings.is_empty() => None,
        Ok(report) => Some(format!(
            "{}\n{}\nPublished history was lost on the storage remote. This did not block your command; repair it as printed above.",
            report.summary(remote),
            report.findings.join("\n")
        )),
        Err(error) => Some(format!("storage guard could not check {remote}: {error}")),
    }
}
