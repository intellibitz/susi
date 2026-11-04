//! Hyper connection settings shared by the hyper-served SUSI surfaces
//! (GEMI REST, GMCP).

use hyper_util::rt::{TokioExecutor, TokioTimer};
use hyper_util::server::conn::auto::Builder;

/// A client must finish sending request headers within this window;
/// without a timer hyper waits forever on a client trickling bytes.
pub const HEADER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Connection builder with the header-read bound applied.
pub fn connection_builder() -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn header_read_timeout_is_thirty_seconds_and_builder_initializes() {
        assert_eq!(HEADER_READ_TIMEOUT, std::time::Duration::from_secs(30));
        let _builder = connection_builder();
    }

    #[test]
    fn header_read_timeout_millis_is_nonzero_thirty_thousand() {
        assert_eq!(HEADER_READ_TIMEOUT.as_millis(), 30_000);
        assert!(!HEADER_READ_TIMEOUT.is_zero());
    }

    #[test]
    fn header_read_timeout_subsecond_nanos_are_zero_and_secs_match_thirty() {
        assert_eq!(HEADER_READ_TIMEOUT.subsec_nanos(), 0);
        assert_eq!(HEADER_READ_TIMEOUT.as_secs(), 30);
    }

    #[tokio::test]
    async fn multiple_connection_builders_construct_independently_in_tokio_runtime() {
        let _first = connection_builder();
        let _second = connection_builder();
    }
}
