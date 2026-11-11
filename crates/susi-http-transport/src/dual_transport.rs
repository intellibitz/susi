//! Dual-protocol transport shared by every SUSI HTTP surface (GEMI REST,
//! GMCP, A2A).
//!
//! Each accepted socket sniffs its first byte. A TLS ClientHello (`0x16`) is
//! served over TLS when an acceptor is configured; anything else is plain
//! HTTP — internal `http://127.0.0.1` callers are unaffected.

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub enum MaybeTls {
    Plain(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>),
}

impl AsyncRead for MaybeTls {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTls {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_shutdown(cx),
        }
    }
    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write_vectored(cx, bufs),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_write_vectored(cx, bufs),
        }
    }
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Plain(s) => s.is_write_vectored(),
            Self::Tls(s) => s.is_write_vectored(),
        }
    }
}

/// Decide the transport for one accepted connection. Every socket sniffs the
/// first byte — a TLS ClientHello (`0x16`) is upgraded when a certificate is
/// configured, anything else is plain HTTP. Sniffing (rather than a
/// destination-based split) keeps the surface proxy-compatible: TLS can be
/// terminated upstream, and a specific external bind still serves plaintext
/// to peers that need it.
///
/// Plaintext policy is peer-based: loopback always passes; off-host
/// (`remote`) plaintext is refused when `require_tls_remote` is set, which
/// the daemon always does (`allow_remote_plaintext` is retired; Susi
/// machines talk over the sealed peer channel) — so TLS guards the external
/// surface while `http://127.0.0.1` keeps working.
///
/// `surface` prefixes handshake-failure logs (e.g. `"[A2A]"`). Returns
/// `None` when the connection must be dropped.
pub async fn negotiate_transport(
    stream: tokio::net::TcpStream,
    remote: bool,
    tls: Option<&tokio_rustls::TlsAcceptor>,
    require_tls_remote: bool,
    surface: &str,
) -> Option<MaybeTls> {
    bounded(
        NEGOTIATE_TIMEOUT,
        surface,
        negotiate_unbounded(stream, remote, tls, require_tls_remote, surface),
    )
    .await
}

/// Bound on the first-byte sniff plus TLS handshake. Without it a client
/// that connects and sends nothing holds its task and file descriptor
/// forever — a slowloris that exhausts fds on the public port.
const NEGOTIATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs a negotiation under `limit`; `None` (connection dropped) on expiry.
pub async fn bounded(
    limit: std::time::Duration,
    surface: &str,
    negotiation: impl std::future::Future<Output = Option<MaybeTls>>,
) -> Option<MaybeTls> {
    let negotiated = tokio::time::timeout(limit, negotiation).await;
    if negotiated.is_err() {
        eprintln!("{surface} dropped connection: no handshake within {limit:?}");
    }
    negotiated.ok().flatten()
}

pub async fn negotiate_unbounded(
    stream: tokio::net::TcpStream,
    remote: bool,
    tls: Option<&tokio_rustls::TlsAcceptor>,
    require_tls_remote: bool,
    surface: &str,
) -> Option<MaybeTls> {
    let mut probe = [0u8; 1];
    let is_tls = matches!(stream.peek(&mut probe).await, Ok(1) if probe[0] == 0x16);
    if is_tls {
        let acceptor = tls?;
        return match acceptor.accept(stream).await {
            Ok(s) => Some(MaybeTls::Tls(Box::new(s))),
            Err(e) => {
                eprintln!("{surface} TLS handshake failed: {e}");
                None
            }
        };
    }
    if remote && require_tls_remote {
        return None;
    }
    Some(MaybeTls::Plain(stream))
}

/// [`negotiate_transport`] for a blocking server ([`crate::server`]): the
/// same first-byte sniff and plaintext policy, a TLS handshake on the same
/// platform stack (`native-tls`) with the acceptor's identity, and the same
/// [`NEGOTIATE_TIMEOUT`] bound on sniff plus handshake together. Runs on the
/// connection's own thread; returns `None` when the connection must be
/// dropped, and otherwise leaves the socket with no timeouts.
#[must_use]
pub fn negotiate_blocking(
    stream: std::net::TcpStream,
    remote: bool,
    tls: Option<&tokio_rustls::TlsAcceptor>,
    require_tls_remote: bool,
    surface: &str,
) -> Option<crate::server::Io> {
    let started = std::time::Instant::now();
    let dropped = || {
        eprintln!("{surface} dropped connection: no handshake within {NEGOTIATE_TIMEOUT:?}");
        None
    };
    if stream.set_read_timeout(Some(NEGOTIATE_TIMEOUT)).is_err() {
        return None;
    }
    let mut probe = [0u8; 1];
    let is_tls = match stream.peek(&mut probe) {
        Ok(1) => probe[0] == 0x16,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return dropped();
        }
        Ok(_) | Err(_) => false,
    };
    if stream.set_read_timeout(None).is_err() {
        return None;
    }
    if !is_tls {
        if remote && require_tls_remote {
            return None;
        }
        return Some(crate::server::Io::Plain(stream));
    }
    let acceptor = tls?;
    // The handshake's share of the bound: a watchdog shuts the socket when
    // it runs out, which fails the handshake's blocked read or write.
    let remaining = NEGOTIATE_TIMEOUT.saturating_sub(started.elapsed());
    let expired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let watched = stream.try_clone().ok()?;
    let fired = std::sync::Arc::clone(&expired);
    std::thread::Builder::new()
        .name("susi-tls-handshake-bound".into())
        .spawn(move || {
            if matches!(
                finished.recv_timeout(remaining),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                fired.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = watched.shutdown(std::net::Shutdown::Both);
            }
        })
        .ok()?;
    let accepted = acceptor.accept_blocking(stream);
    drop(done);
    if expired.load(std::sync::atomic::Ordering::SeqCst) {
        return dropped();
    }
    match accepted {
        Ok(stream) => Some(crate::server::Io::Tls(Box::new(stream))),
        Err(e) => {
            eprintln!("{surface} TLS handshake failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let client_fut = TcpStream::connect(addr);
        let accept_fut = listener.accept();
        let (client, accepted) = tokio::join!(client_fut, accept_fut);
        (client.expect("connect"), accepted.expect("accept").0)
    }

    #[tokio::test]
    async fn loopback_plaintext_passes_with_require_tls_remote_and_roundtrips_async_io() {
        let (mut client, server) = connected_pair().await;
        client
            .write_all(b"GET /health HTTP/1.1\r\n\r\n")
            .await
            .expect("write probe");

        let negotiated = negotiate_unbounded(server, false, None, true, "[TEST]")
            .await
            .expect("loopback plaintext must pass even when require_tls_remote is true");
        assert!(matches!(negotiated, MaybeTls::Plain(_)));

        let mut transport = negotiated;
        let mut req_buf = [0u8; 24];
        transport
            .read_exact(&mut req_buf)
            .await
            .expect("read through MaybeTls");
        assert_eq!(&req_buf, b"GET /health HTTP/1.1\r\n\r\n");

        transport
            .write_all(b"HTTP/1.1 200 OK\r\n\r\n")
            .await
            .expect("write through MaybeTls");
        transport.flush().await.expect("flush MaybeTls");
        transport.shutdown().await.expect("shutdown MaybeTls");

        let mut resp = Vec::new();
        client
            .read_to_end(&mut resp)
            .await
            .expect("read client reply");
        assert_eq!(resp, b"HTTP/1.1 200 OK\r\n\r\n");
    }

    #[tokio::test]
    async fn remote_plaintext_policy_and_unconfigured_tls_probe_drop_cleanly() {
        // 1. Remote plaintext refused when require_tls_remote = true
        let (mut c1, s1) = connected_pair().await;
        c1.write_all(b"GET / HTTP/1.1\r\n\r\n")
            .await
            .expect("write c1");
        assert!(negotiate_unbounded(s1, true, None, true, "[TEST]")
            .await
            .is_none());

        // 2. Remote plaintext allowed when require_tls_remote = false
        let (mut c2, s2) = connected_pair().await;
        c2.write_all(b"GET / HTTP/1.1\r\n\r\n")
            .await
            .expect("write c2");
        assert!(matches!(
            negotiate_unbounded(s2, true, None, false, "[TEST]").await,
            Some(MaybeTls::Plain(_))
        ));

        // 3. TLS ClientHello (0x16) dropped when no TlsAcceptor is configured
        let (mut c3, s3) = connected_pair().await;
        c3.write_all(&[0x16, 0x03, 0x01]).await.expect("write c3");
        assert!(negotiate_unbounded(s3, false, None, false, "[TEST]")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn bounded_drops_slowloris_connection_sending_no_first_byte() {
        let (_idle_client, server) = connected_pair().await;
        let result = bounded(
            Duration::from_millis(25),
            "[TEST]",
            negotiate_unbounded(server, false, None, false, "[TEST]"),
        )
        .await;
        assert!(result.is_none(), "idle connection must time out and drop");
    }

    #[tokio::test]
    async fn maybe_tls_plain_vectored_write_and_negotiate_transport_wrapper_succeeds_on_loopback() {
        let (mut client, server) = connected_pair().await;
        client.write_all(b"G").await.expect("write initial byte");

        let mut transport = negotiate_transport(server, false, None, true, "[TEST]")
            .await
            .expect("negotiate_transport on loopback must succeed");
        assert!(matches!(transport, MaybeTls::Plain(_)));
        let _ = transport.is_write_vectored();

        let mut probe = [0u8; 1];
        transport.read_exact(&mut probe).await.expect("read probe");
        assert_eq!(&probe, b"G");

        let slices = [
            std::io::IoSlice::new(b"vec-"),
            std::io::IoSlice::new(b"payload"),
        ];
        let written = transport
            .write_vectored(&slices)
            .await
            .expect("write_vectored");
        if written < 11 {
            transport
                .write_all(&b"vec-payload"[written..])
                .await
                .expect("write remainder");
        }
        transport.flush().await.expect("flush");
        transport.shutdown().await.expect("shutdown");

        let mut received = Vec::new();
        client
            .read_to_end(&mut received)
            .await
            .expect("read vectored output");
        assert_eq!(received, b"vec-payload");
    }

    #[test]
    fn negotiate_timeout_constant_is_ten_seconds() {
        assert_eq!(NEGOTIATE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(NEGOTIATE_TIMEOUT.as_millis(), 10_000);
        assert!(!NEGOTIATE_TIMEOUT.is_zero());
    }
}
