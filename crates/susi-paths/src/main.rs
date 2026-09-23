//! Standalone `susi-paths` REST service — thin shell over
//! `susi_paths::serve`; the same entry is dispatched in-process by the
//! root `susi` binary's `service-run` mode.
//! Default bind: `127.0.0.1:18080` (override with `SUSI_PATHS_PORT`).

const DEFAULT_PORT: u16 = 18080;

fn main() -> std::io::Result<()> {
    let port = std::env::var("SUSI_PATHS_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    susi_paths::serve(port)
}
