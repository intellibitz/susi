//! Hot-Reloading of WASM Plugins (Swarm OS Bullet 27)
//!
//! Validates a candidate module with `wasmtime` before it replaces the
//! live one, so a malformed or incompatible binary can never take down a
//! running plugin slot — the swap only happens once compilation proves
//! the bytes are a loadable module.

use std::collections::HashMap;
use std::sync::RwLock;

use wasmtime::{Engine, Module};

pub struct PluginReloader {
    engine: Engine,
    loaded: RwLock<HashMap<String, Vec<u8>>>,
}

impl Default for PluginReloader {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginReloader {
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
            loaded: RwLock::new(HashMap::new()),
        }
    }

    /// Validates `wasm_bytes` and, only if it compiles cleanly, swaps it in
    /// for `plugin_id`. Rejects the swap (keeping the previous binary live)
    /// on any compile error.
    pub fn reload_plugin(&self, plugin_id: &str, wasm_bytes: &[u8]) -> Result<(), String> {
        Module::from_binary(&self.engine, wasm_bytes).map_err(|e| e.to_string())?;
        self.loaded
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin_id.to_string(), wasm_bytes.to_vec());
        Ok(())
    }

    pub fn get_loaded(&self, plugin_id: &str) -> Option<Vec<u8>> {
        self.loaded
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(plugin_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The minimal valid WASM module: magic number + version, no sections.
    const EMPTY_MODULE: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    #[test]
    fn valid_module_swaps_in() {
        let reloader = PluginReloader::new();
        reloader.reload_plugin("plugin-a", EMPTY_MODULE).unwrap();
        assert_eq!(reloader.get_loaded("plugin-a"), Some(EMPTY_MODULE.to_vec()));
    }

    #[test]
    fn garbage_bytes_are_rejected_and_dont_clobber_the_live_module() {
        let reloader = PluginReloader::new();
        reloader.reload_plugin("plugin-a", EMPTY_MODULE).unwrap();
        assert!(reloader.reload_plugin("plugin-a", b"not wasm").is_err());
        assert_eq!(reloader.get_loaded("plugin-a"), Some(EMPTY_MODULE.to_vec()));
    }
}
