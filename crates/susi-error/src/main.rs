//! Standalone `susi-error` REST service — thin shell over
//! `susi_error::serve`; the same entry is dispatched in-process by the
//! root `susi` binary's `service-run` mode.
//! Default bind: `127.0.0.1:18081` (override with `SUSI_ERROR_PORT`).

const DEFAULT_PORT: u16 = 18081;

fn main() -> std::io::Result<()> {
    let port = std::env::var("SUSI_ERROR_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    susi_error::serve(port)
}
