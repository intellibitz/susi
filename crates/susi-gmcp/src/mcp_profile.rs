//! MCP spec versions, negotiation and registry metadata (VC-201-086 / T-CLAUDE-273).

use serde::{Deserialize, Serialize};

use crate::mcp_probe::{McpProbe, McpProbeOptions, McpProbeReport, McpProbeTransport};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSpecProfile {
    pub versions: Vec<String>,
    pub negotiated: Option<String>,
    pub registry_metadata: serde_json::Value,
}

impl McpSpecProfile {
    #[must_use]
    pub fn default_profile() -> Self {
        Self {
            versions: crate::mcp_probe::SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .map(|version| (*version).to_string())
                .collect(),
            negotiated: None,
            registry_metadata: serde_json::json!({
                "registry": "mcp",
                "probe_status": "unprobed",
                "supports_list_changed": null
            }),
        }
    }

    pub fn negotiate(&mut self, peer_versions: &[&str]) -> Option<String> {
        let chosen = self
            .versions
            .iter()
            .rev()
            .find(|v| peer_versions.iter().any(|p| p == v))
            .cloned();
        self.negotiated = chosen.clone();
        chosen
    }

    /// Probe a live MCP transport and persist the observed compatibility
    /// facts in this profile.  Catalog metadata remains `unprobed` until this
    /// method is called; a failed negative-path check is recorded as failed
    /// rather than being mistaken for an unsupported catalog entry.
    pub fn probe<T: McpProbeTransport>(
        &mut self,
        transport: &mut T,
        options: McpProbeOptions,
    ) -> McpProbeReport {
        let report = McpProbe::new(options).run(transport);
        self.record_probe(&report);
        report
    }

    pub fn record_probe(&mut self, report: &McpProbeReport) {
        self.negotiated = report.protocol_version.as_deref().and_then(|version| {
            self.versions
                .iter()
                .find(|candidate| candidate.as_str() == version)
                .cloned()
        });
        let checks = serde_json::to_value(&report.checks).unwrap_or(serde_json::Value::Null);
        self.registry_metadata = serde_json::json!({
            "registry": "mcp",
            "probe_status": if report.passed() { "passed" } else { "failed" },
            "protocol_version": report.protocol_version,
            "server_info": report.server_info,
            "supports_list_changed": report.supports_list_changed,
            "discovered_tools": report.tools,
            "checks": checks
        });
    }
}

#[cfg(test)]
#[path = "tests/mcp_profile.rs"]
mod eco_mcp_versions_tests;
