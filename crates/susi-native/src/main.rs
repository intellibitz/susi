//! Standalone `susi-native` REST service — thin shell over
//! `susi_native::serve`; the same entry is dispatched in-process by the
//! root `susi` binary's `service-run` mode.
//! Default bind: `127.0.0.1:18084` (override with `SUSI_NATIVE_PORT`).

const DEFAULT_PORT: u16 = 18084;

/// The instance offset — duplicated from `susi_paths::ports::env_port_offset`
/// because this leaf is dependency-free (it vendors nothing). Keep the parse
/// identical: trim + u16, invalid → 0.
fn env_port_offset() -> u16 {
    std::env::var("SUSI_PORT_OFFSET")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(0)
}

fn main() -> std::io::Result<()> {
    let port = std::env::var("SUSI_NATIVE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or_else(|| DEFAULT_PORT.saturating_add(env_port_offset()));
    susi_native::serve(port)
}
