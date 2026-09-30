//! Import MCP server configs without hand-edited JSON.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpImport {
    pub name: String,
    pub command: String,
}

#[must_use]
pub fn parse_mcp_import_lines(text: &str) -> Vec<McpImport> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, cmd)) = line.split_once('=') {
            out.push(McpImport {
                name: name.trim().to_string(),
                command: cmd.trim().to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod zc_mcp_import_tests {
    use super::*;

    #[test]
    fn zc_mcp_import_parses_name_command_pairs() {
        let v = parse_mcp_import_lines("fs=npx @modelcontextprotocol/server-filesystem\n#c\n");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "fs");
    }
}
