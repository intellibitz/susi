//! Standalone `susi-paths` REST service — thin shell over
//! `susi_paths::serve`; the same entry is dispatched in-process by the
//! root `susi` binary's `service-run` mode.
//! Default bind: `127.0.0.1:18080` (override with `SUSI_PATHS_PORT`;
//! the default itself shifts by `SUSI_PORT_OFFSET` for a second instance).

const DEFAULT_PORT: u16 = 18080;

fn main() -> std::io::Result<()> {
    let port = std::env::var("SUSI_PATHS_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or_else(|| DEFAULT_PORT.saturating_add(susi_paths::ports::env_port_offset()));
    susi_paths::serve(port)
}
