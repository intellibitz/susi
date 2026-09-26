//! Dual-protocol transport shared by every SUSI HTTP surface (GEMI REST,
//! GMCP, A2A). Canonical source: `crates/susi-server/src/dual_transport.rs`,
//! `#[path]`-mounted by the other servers.
//!
//! Each accepted socket sniffs its first byte. A TLS ClientHello (`0x16`) is
//! served over TLS when an acceptor is configured; anything else is plain
//! HTTP — internal `http://127.0.0.1` callers are unaffected.

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(crate) enum MaybeTls {
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
/// (`remote`) plaintext is refused only when `require_tls_remote`
/// (config `https_only`) is set — so an operator can force TLS on the
/// external surface while internal `http://127.0.0.1` callers keep working.
///
/// `surface` prefixes handshake-failure logs (e.g. `"[A2A]"`). Returns
/// `None` when the connection must be dropped.
pub(crate) async fn negotiate_transport(
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
