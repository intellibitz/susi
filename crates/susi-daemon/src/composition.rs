//! Composition roots: the only places that assemble concrete adapters.
//!
//! - CLI: [`wire_cli_substrate`]
//! - Daemon: [`wire_engine_hooks`] then [`crate::auto_discovery::bootstrap_zero_config_substrate`]
//!
//! See `ARCHITECTURE.md` at the repo root.

use std::path::Path;

/// Wire `EngineHooks` so `ToolRegistry` and `susi-gmcp` tool handlers can reach
/// gawd/gemi without crate cycles. Safe to call more than once; later calls
/// are ignored by `OnceLock`.
pub fn wire_engine_hooks() {
    susi_tools::hooks::init(Box::new(crate::engine_hooks::SusiEngineHooks));
}

/// CLI composition root: hooks → extension packs → cloud.env → auto-prime.
///
/// Does not bind host-contract ports (daemon owns those). Call before command
/// dispatch that may touch tools, catalogs, or inference.
pub fn wire_cli_substrate(substrate: &Path) {
    wire_engine_hooks();
    let _ = susi_sandbox::extensions::ensure_extensions_substrate();
    susi_gemi::http_provider::apply_cloud_env_file();
    let _ = std::fs::create_dir_all(substrate);
    susi_core::context_graph::ContextGraph::init_global_storage(
        substrate.join("context_graph.jsonl"),
    );
    crate::privacy::wire_mac_policy(substrate);
    crate::ambient::start_ambient_indexer(substrate);
    crate::auto_discovery::auto_prime_ecosystem(substrate);
}
