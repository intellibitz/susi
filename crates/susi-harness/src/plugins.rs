//! The plugin manifest: providers, tools and agents declared, not compiled.
//!
//! A manifest names what a plugin provides and which host capabilities it
//! needs. Installing it registers its tools with the tool registry; launching
//! it starts the MCP server. A plugin that asks for a capability the host does
//! not grant is refused before any process is spawned — the manifest is a
//! capability list, not a promise.
//!
//! The document is JSON (`harness.plugins.json`): this crate declares no TOML
//! dependency, and data in this repository is JSON. The shape is stable:
//!
//! ```json
//! {"plugins": [{
//!   "name": "web", "kind": "mcp", "command": "npx",
//!   "args": ["-y", "server-everything"],
//!   "capabilities": ["tools", "network"],
//!   "tools": [{"name": "web_search", "policy": "ask"}]
//! }]}
//! ```

use crate::mcp::{McpClient, StdioTransport};
use crate::tools::{Policy, Registry, ToolSpec};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

/// Capabilities the host is willing to grant.
pub const HOST_CAPABILITIES: [&str; 3] = ["filesystem", "network", "tools"];

/// What a plugin is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    /// An MCP server, launched as a child process.
    Mcp,
    /// A WASM component. Parsed, but no host runs it yet.
    Wasm,
}

/// A tool a plugin provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidedTool {
    /// Tool name.
    pub name: String,
    /// Authority the tool carries.
    pub policy: Policy,
}

/// One declared plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    /// Unique name.
    pub name: String,
    /// What it is.
    pub kind: PluginKind,
    /// Executable to launch (empty for a component).
    pub command: String,
    /// Arguments for the executable.
    pub args: Vec<String>,
    /// Host capabilities it needs.
    pub capabilities: Vec<String>,
    /// Tools it provides.
    pub tools: Vec<ProvidedTool>,
}

/// A parsed manifest.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    plugins: Vec<Plugin>,
}

impl Manifest {
    /// Parse a manifest document.
    ///
    /// # Errors
    ///
    /// Returns a config error for malformed JSON, a missing name, an unknown
    /// kind, a duplicate plugin or tool, an unknown capability, or a tool
    /// without the `tools` capability.
    pub fn parse(json: &str) -> EaiResult<Self> {
        let root: Value = serde_json::from_str(json)
            .map_err(|error| EaiError::config(format!("plugin manifest is not JSON: {error}")))?;
        let raw = root
            .get("plugins")
            .and_then(Value::as_array)
            .ok_or_else(|| EaiError::config("plugin manifest has no plugins array"))?;
        let mut plugins: Vec<Plugin> = Vec::new();
        let mut names = BTreeSet::new();
        let mut tools = BTreeSet::new();
        for entry in raw {
            let plugin = parse_plugin(entry)?;
            if !names.insert(plugin.name.clone()) {
                return Err(EaiError::config(format!(
                    "plugin `{}` is declared twice",
                    plugin.name
                )));
            }
            for tool in &plugin.tools {
                if !tools.insert(tool.name.clone()) {
                    return Err(EaiError::config(format!(
                        "tool `{}` is provided twice",
                        tool.name
                    )));
                }
            }
            plugins.push(plugin);
        }
        Ok(Self { plugins })
    }

    /// Load a manifest from disk.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file cannot be read, or the parse error.
    pub fn load(path: &Path) -> EaiResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            EaiError::io(format!("read plugin manifest {}: {error}", path.display()))
        })?;
        Self::parse(&text)
    }

    /// The declared plugins.
    #[must_use]
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    /// Register every MCP tool the manifest provides.
    ///
    /// # Errors
    ///
    /// Returns a config error for a WASM plugin, which this build cannot run.
    pub fn install(&self, registry: &mut Registry) -> EaiResult<usize> {
        let mut installed = 0;
        for plugin in &self.plugins {
            if plugin.kind == PluginKind::Wasm {
                return Err(EaiError::config(format!(
                    "plugin `{}` is a WASM component; this build has no WASM host yet",
                    plugin.name
                )));
            }
            for tool in &plugin.tools {
                registry.register(ToolSpec::mcp(
                    &tool.name,
                    &format!("Provided by plugin `{}`", plugin.name),
                    json!({"type": "object"}),
                    tool.policy,
                ));
                installed += 1;
            }
        }
        Ok(installed)
    }

    /// Start one MCP plugin.
    ///
    /// # Errors
    ///
    /// Returns a config error for an unknown or non-MCP plugin, or an I/O
    /// error when the server cannot be spawned.
    pub fn launch(&self, name: &str) -> EaiResult<McpClient<StdioTransport>> {
        let plugin = self
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .ok_or_else(|| EaiError::config(format!("no plugin `{name}`")))?;
        if plugin.kind != PluginKind::Mcp {
            return Err(EaiError::config(format!(
                "plugin `{name}` is not an MCP plugin"
            )));
        }
        Ok(McpClient::new(StdioTransport::spawn(
            &plugin.command,
            &plugin.args,
        )?))
    }
}

fn parse_plugin(raw: &Value) -> EaiResult<Plugin> {
    let name = string_field(raw, "name")?;
    let kind = match string_field(raw, "kind")?.as_str() {
        "mcp" => PluginKind::Mcp,
        "wasm" => PluginKind::Wasm,
        other => {
            return Err(EaiError::config(format!(
                "plugin `{name}` has unknown kind `{other}`"
            )))
        }
    };
    let command = raw
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if kind == PluginKind::Mcp && command.is_empty() {
        return Err(EaiError::config(format!(
            "plugin `{name}` has no command to launch"
        )));
    }
    let args = strings(raw, "args");
    let capabilities = strings(raw, "capabilities");
    for capability in &capabilities {
        if !HOST_CAPABILITIES.contains(&capability.as_str()) {
            return Err(EaiError::config(format!(
                "plugin `{name}` asks for capability `{capability}`, which the host does not grant"
            )));
        }
    }
    let mut tools = Vec::new();
    if let Some(list) = raw.get("tools").and_then(Value::as_array) {
        for tool in list {
            let tool_name = string_field(tool, "name")?;
            let policy = parse_policy(tool.get("policy").and_then(Value::as_str).unwrap_or("ask"))?;
            tools.push(ProvidedTool {
                name: tool_name,
                policy,
            });
        }
    }
    if !tools.is_empty() && !capabilities.iter().any(|value| value == "tools") {
        return Err(EaiError::config(format!(
            "plugin `{name}` provides tools but does not declare the `tools` capability"
        )));
    }
    Ok(Plugin {
        name,
        kind,
        command,
        args,
        capabilities,
        tools,
    })
}

fn parse_policy(value: &str) -> EaiResult<Policy> {
    match value {
        "auto" => Ok(Policy::Auto),
        "ask" => Ok(Policy::Ask),
        "deny" => Ok(Policy::Deny),
        other => Err(EaiError::config(format!(
            "unknown tool policy `{other}`; use auto, ask or deny"
        ))),
    }
}

fn strings(raw: &Value, field: &str) -> Vec<String> {
    raw.get(field)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn string_field(raw: &Value, field: &str) -> EaiResult<String> {
    raw.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| EaiError::config(format!("plugin manifest: `{field}` is required")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Handler;

    const MANIFEST: &str = r#"
    {
      "plugins": [
        {
          "name": "web",
          "kind": "mcp",
          "command": "npx",
          "args": ["-y", "server-everything"],
          "capabilities": ["tools", "network"],
          "tools": [
            {"name": "web_search", "policy": "ask"},
            {"name": "fetch_page", "policy": "ask"}
          ]
        }
      ]
    }
    "#;

    #[test]
    fn plugin_manifest_parses_and_registers_mcp_tools() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(manifest.plugins().len(), 1);
        let mut registry = Registry::builtin();
        assert_eq!(manifest.install(&mut registry).unwrap(), 2);
        let tool = registry.get("web_search").unwrap();
        assert_eq!(tool.handler, Handler::Mcp);
        assert_eq!(tool.policy, Policy::Ask);
    }

    #[test]
    fn plugin_manifest_refuses_an_undeclared_capability() {
        let json = r#"{"plugins":[{"name":"x","kind":"mcp","command":"x",
            "capabilities":["gpu"]}]}"#;
        let error = Manifest::parse(json).unwrap_err().to_string();
        assert!(error.contains("gpu"), "{error}");
    }

    #[test]
    fn plugin_manifest_requires_the_tools_capability() {
        let json = r#"{"plugins":[{"name":"x","kind":"mcp","command":"x",
            "capabilities":["network"],
            "tools":[{"name":"t"}]}]}"#;
        let error = Manifest::parse(json).unwrap_err().to_string();
        assert!(error.contains("tools"), "{error}");
    }

    #[test]
    fn plugin_manifest_refuses_duplicate_names_and_tools() {
        let duplicate_plugin = r#"{"plugins":[
            {"name":"x","kind":"mcp","command":"x"},
            {"name":"x","kind":"mcp","command":"x"}]}"#;
        assert!(Manifest::parse(duplicate_plugin).is_err());
        let duplicate_tool = r#"{"plugins":[
            {"name":"x","kind":"mcp","command":"x","capabilities":["tools"],
             "tools":[{"name":"t"}]},
            {"name":"y","kind":"mcp","command":"y","capabilities":["tools"],
             "tools":[{"name":"t"}]}]}"#;
        assert!(Manifest::parse(duplicate_tool).is_err());
    }

    #[test]
    fn plugin_manifest_refuses_a_wasm_component_until_a_host_exists() {
        let json = r#"{"plugins":[{"name":"x","kind":"wasm","capabilities":[]}]}"#;
        let manifest = Manifest::parse(json).unwrap();
        let mut registry = Registry::builtin();
        let error = manifest.install(&mut registry).unwrap_err().to_string();
        assert!(error.contains("WASM"), "{error}");
    }

    #[test]
    fn plugin_manifest_example_parses() {
        let example = include_str!("../../../deploy/harness/plugins.example.json");
        let manifest = Manifest::parse(example).unwrap();
        assert!(!manifest.plugins().is_empty());
        let mut registry = Registry::builtin();
        assert!(manifest.install(&mut registry).unwrap() > 0);
    }
}
