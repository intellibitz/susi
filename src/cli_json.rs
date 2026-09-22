//! Shared CLI JSON printing with secret redaction.

use anyhow::Result;

/// Pretty-print a serializable value to stdout after redacting secret-shaped tokens.
pub fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_agents::external::redact(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}
