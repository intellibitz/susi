//! WebAssembly Host Plugin System (Swarm OS Bullet 48)
//!
//! Provides a system where cells can dynamically load or interact with
//! proprietary binary parsers and plugins on the host side.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

/// Metadata for a dynamically loaded plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities_provided: Vec<String>,
}

/// A mock trait representing a loaded host plugin.
pub trait HostPlugin: Send + Sync {
    fn metadata(&self) -> PluginMetadata;
    fn execute(&self, input: &[u8]) -> Result<Vec<u8>, String>;
}

/// Manages dynamically loaded binary parsers and host plugins.
pub struct PluginManager {
    plugins: RwLock<HashMap<String, Box<dyn HostPlugin>>>,
    plugin_dir: PathBuf,
}

impl PluginManager {
    pub fn new(workspace: &std::path::Path) -> Self {
        let plugin_dir = workspace.join("plugins");
        if !plugin_dir.exists() {
            let _ = std::fs::create_dir_all(&plugin_dir);
        }
        Self {
            plugins: RwLock::new(HashMap::new()),
            plugin_dir,
        }
    }

    /// Registers a new plugin manually (e.g., from host code).
    pub fn register_plugin(&self, plugin: Box<dyn HostPlugin>) {
        let name = plugin.metadata().name.clone();
        self.plugins
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, plugin);
    }

    /// Invokes a plugin by name with the given input payload.
    pub fn invoke_plugin(&self, name: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        let map = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        if let Some(plugin) = map.get(name) {
            plugin.execute(input)
        } else {
            Err(format!("Plugin not found: {}", name))
        }
    }

    /// Returns a list of all currently loaded plugins and their metadata.
    pub fn list_plugins(&self) -> Vec<PluginMetadata> {
        let map = self.plugins.read().unwrap_or_else(|e| e.into_inner());
        map.values().map(|p| p.metadata()).collect()
    }

    /// Returns the absolute path to the plugin directory.
    pub fn plugin_dir(&self) -> &std::path::Path {
        &self.plugin_dir
    }
}

// ──────────────────────────────────────────────────────────
// Mock Example Plugin
// ──────────────────────────────────────────────────────────

/// A mock binary parser plugin (e.g. for parsing proprietary PDF/binary formats).
pub struct MockBinaryParser;

impl HostPlugin for MockBinaryParser {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "pdf_parser".to_string(),
            version: "1.0.0".to_string(),
            description: "Proprietary PDF to Text extraction".to_string(),
            capabilities_provided: vec!["parser:pdf".to_string()],
        }
    }

    fn execute(&self, _input: &[u8]) -> Result<Vec<u8>, String> {
        Ok(b"extracted text from proprietary binary".to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_system() {
        let temp_dir = std::env::temp_dir();
        let manager = PluginManager::new(&temp_dir);

        manager.register_plugin(Box::new(MockBinaryParser));

        let plugins = manager.list_plugins();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "pdf_parser");

        let output = manager
            .invoke_plugin("pdf_parser", b"dummy_pdf_data")
            .unwrap();
        assert_eq!(output, b"extracted text from proprietary binary");

        let missing = manager.invoke_plugin("unknown_parser", b"data");
        assert!(missing.is_err());
    }
}
