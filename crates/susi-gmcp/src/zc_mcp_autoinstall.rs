//! Auto-install known MCP servers when referenced.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpInstallPlan {
    pub package: String,
    pub already_present: bool,
}

#[must_use]
pub fn autoinstall_plan(name: &str, installed: &[&str]) -> McpInstallPlan {
    let package = format!("mcp-server-{name}");
    McpInstallPlan {
        already_present: installed.iter().any(|i| *i == package),
        package,
    }
}

#[cfg(test)]
mod zc_mcp_autoinstall_tests {
    use super::*;

    #[test]
    fn zc_mcp_autoinstall_skips_when_present() {
        let p = autoinstall_plan("fs", &["mcp-server-fs"]);
        assert!(p.already_present);
        let q = autoinstall_plan("git", &[]);
        assert!(!q.already_present);
        assert_eq!(q.package, "mcp-server-git");
    }
}
