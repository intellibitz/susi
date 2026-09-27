//! Hyper connection settings shared by the hyper-served SUSI surfaces
//! (GEMI REST, GMCP). Canonical source: `crates/susi-server/src/http_conn.rs`,
//! `#[path]`-mounted by `susi-gmcp`.

use hyper_util::rt::{TokioExecutor, TokioTimer};
use hyper_util::server::conn::auto::Builder;

/// A client must finish sending request headers within this window;
/// without a timer hyper waits forever on a client trickling bytes.
const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Connection builder with the header-read bound applied.
pub(crate) fn connection_builder() -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder
}
