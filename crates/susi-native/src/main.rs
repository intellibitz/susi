//! Standalone `susi-native` REST service — thin shell over
//! `susi_native::serve`; the same entry is dispatched in-process by the
//! root `susi` binary's `service-run` mode.
//! Default bind: `127.0.0.1:18084` (override with `SUSI_NATIVE_PORT`).

const DEFAULT_PORT: u16 = 18084;

fn main() -> std::io::Result<()> {
    let port = std::env::var("SUSI_NATIVE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    susi_native::serve(port)
}
