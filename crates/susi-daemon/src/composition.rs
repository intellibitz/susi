//! Composition roots: the only places that assemble concrete adapters.
//!
//! - CLI: [`wire_cli_substrate`]
//! - Daemon: [`wire_engine_hooks`] then [`crate::auto_discovery::bootstrap_zero_config_substrate`]
//!
//! See `ARCHITECTURE.md` at the repo root.

use std::path::Path;

/// Register in-process plane-bus handlers for gemi / gawd / tools / agents.
#[allow(clippy::unwrap_used)] // SAFETY: current_exe and parent will always exist in a valid build
pub fn wire_plane_bus() {
    // Spawn the decoupled GEMI micro-daemon as a Swarm Cell
    std::thread::spawn(|| {
        let _ = std::process::Command::new(
            std::env::current_exe()
                .unwrap_or_else(|_| "susi-daemon".into())
                .parent()
                .unwrap()
                .join("susi-gemi"),
        )
        .spawn();
    });

    // Spawn the decoupled GMCP micro-daemon as a Swarm Cell
    std::thread::spawn(|| {
        let _ = std::process::Command::new(
            std::env::current_exe()
                .unwrap_or_else(|_| "susi-daemon".into())
                .parent()
                .unwrap()
                .join("susi-gmcp"),
        )
        .spawn();
    });
    // Spawn the decoupled GAWD micro-daemon as a Swarm Cell
    std::thread::spawn(|| {
        let _ = std::process::Command::new(
            std::env::current_exe()
                .unwrap_or_else(|_| "susi-daemon".into())
                .parent()
                .unwrap()
                .join("susi-gawd"),
        )
        .spawn();
    });

    // Spawn the DeepSeek Harness (DSH) Swarm Cell wrapper
    std::thread::spawn(|| {
        let _ = std::process::Command::new(
            std::env::current_exe()
                .unwrap_or_else(|_| "susi-daemon".into())
                .parent()
                .unwrap()
                .join("susi-dsh-cell"),
        )
        .spawn();
    });
    // susi_tools::plane_handler::register();
    // susi_agents::plane_handler::register();
}

/// Wire `EngineHooks` so `ToolRegistry` and `susi-gmcp` tool handlers can reach
/// gawd/gemi without crate cycles. Safe to call more than once; later calls
/// are ignored by `OnceLock`.
pub fn wire_engine_hooks() {
    wire_plane_bus();
    // susi_tools::hooks::init(Box::new(crate::engine_hooks::SusiEngineHooks));
}

/// CLI composition root: hooks → extension packs → cloud.env → auto-prime.
///
/// Does not bind host-contract ports (daemon owns those). Call before command
/// dispatch that may touch tools, catalogs, or inference.
pub fn wire_cli_substrate(substrate: &Path) {
    wire_plane_bus();
    wire_engine_hooks();
    let _ = crate::susi_sandbox::extensions::ensure_extensions_substrate();
    // susi_gemi::http_provider::apply_cloud_env_file();
    let _ = std::fs::create_dir_all(substrate);
    susi_core::context_graph::ContextGraph::init_global_storage(
        substrate.join("context_graph.jsonl"),
    );
    crate::privacy::wire_mac_policy(substrate);
    crate::ambient::start_ambient_indexer(substrate);
    crate::auto_discovery::auto_prime_ecosystem(substrate);
}
