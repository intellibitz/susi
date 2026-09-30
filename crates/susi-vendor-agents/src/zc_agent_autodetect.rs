//! Detect installed agent CLIs and register them ready-to-use.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedAgent {
    pub name: String,
    pub binary: String,
}

#[must_use]
pub fn detect_agents(which_output: &str) -> Vec<DetectedAgent> {
    let mut out = Vec::new();
    for line in which_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let name = line.rsplit('/').next().unwrap_or(line).to_string();
        out.push(DetectedAgent {
            name,
            binary: line.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod zc_agent_autodetect_tests {
    use super::*;

    #[test]
    fn zc_agent_autodetect_registers_found_clis() {
        let d = detect_agents("/usr/bin/aider\n/opt/bin/claude\n");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].name, "aider");
        assert_eq!(d[1].binary, "/opt/bin/claude");
    }
}
