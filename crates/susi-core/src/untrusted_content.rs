//! Mark tool output as untrusted; resist prompt injection (VC-201-074).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrustedBlob {
    pub source: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    Read,
    Consequential,
}

#[must_use]
pub fn wrap_tool_output(source: &str, body: &str) -> UntrustedBlob {
    UntrustedBlob {
        source: source.to_string(),
        body: body.to_string(),
    }
}

/// Embedded instructions in untrusted data cannot mint permissions.
#[must_use]
pub fn policy_allows(blob: &UntrustedBlob, action: ActionClass) -> bool {
    match action {
        ActionClass::Read => true,
        ActionClass::Consequential => {
            // Any instructional payload in untrusted content is refused for grants.
            let lower = blob.body.to_lowercase();
            !(lower.contains("ignore previous")
                || lower.contains("grant permission")
                || lower.contains("new capability")
                || lower.contains("system:"))
        }
    }
}
