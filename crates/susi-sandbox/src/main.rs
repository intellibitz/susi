//! Standalone `susi-sandbox` REST service — thin shell over
//! `susi_sandbox::serve`; the same entry is dispatched in-process by the
//! root `susi` binary's `service-run` mode.
//! Default bind: `127.0.0.1:18083` (override with `SUSI_SANDBOX_PORT`).

const DEFAULT_PORT: u16 = 18083;

/// The instance offset — mirrors `susi_paths::ports::env_port_offset`
/// (the vendored module is private to the lib). Keep the parse identical:
/// trim + u16, invalid → 0.
fn env_port_offset() -> u16 {
    std::env::var("SUSI_PORT_OFFSET")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(0)
}

fn main() -> std::io::Result<()> {
    let port = std::env::var("SUSI_SANDBOX_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or_else(|| DEFAULT_PORT.saturating_add(env_port_offset()));
    susi_sandbox::serve(port)
}
