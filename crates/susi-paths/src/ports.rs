//! Canonical public substrate ports. External clients may hard-code these;
//! the daemon must never silently drift to ephemeral ports. A *uniform*
//! `port_offset` (config key or `SUSI_PORT_OFFSET` env, env wins) shifts all
//! five together — the contract shape stays fixed while a second instance or
//! a nonstandard host layout gets clean ports (e.g. offset 100 → 9190–9194).
//!
//! Canonical source, `#[path]`-mounted by every crate through
//! `crates/susi-core/src/susi_paths.rs`.

pub const GMCP: u16 = 9090;
pub const GEMI: u16 = 9091;
pub const UDP_DISCOVERY: u16 = 9092;
pub const GMCP_HTTP: u16 = 9093;
pub const A2A_HTTP: u16 = 9094;

/// Stable host contract advertised to external clients.
pub const ALL: [(u16, &str); 5] = [
    (GMCP, "GMCP/MCP HTTP"),
    (GEMI, "GEMI HTTP"),
    (UDP_DISCOVERY, "A2A UDP discovery"),
    (GMCP_HTTP, "GMCP HTTP alias"),
    (A2A_HTTP, "A2A HTTP (JSON-RPC + SSE + agent card)"),
];

/// Offset from `SUSI_PORT_OFFSET` alone — leaf-safe resolution for code
/// without a `SusiConfig` in scope. `SusiConfig::port_offset()` additionally
/// honors the `port_offset` config key; env always wins.
pub fn env_port_offset() -> u16 {
    std::env::var("SUSI_PORT_OFFSET")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(0)
}

/// `base + env_offset`, saturating — never produces a port above u16::MAX.
pub fn effective(base: u16) -> u16 {
    base.saturating_add(env_port_offset())
}
