//! Hyper runtime adapters (`TokioIo`, `TokioExecutor`, `TokioTimer`) and
//! HTTP/1.1 + HTTP/2 auto-detecting connection builder shared by the
//! hyper-served SUSI surfaces (GEMI REST, GMCP) with zero `hyper-util` or
//! `unsafe` dependencies.

use std::future::Future;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

/// HTTP/2 client connection preface (`RFC 9113 §3.4`, 24 octets).
const H2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// A client must finish sending request headers within this window;
/// without a timer hyper waits forever on a client trickling bytes.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Hyper future executor backed by [`tokio::spawn`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioExecutor;

impl TokioExecutor {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl<F> hyper::rt::Executor<F> for TokioExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

/// Hyper [`Timer`](hyper::rt::Timer) backed by [`tokio::time`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioTimer;

impl TokioTimer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

struct TokioSleep(Pin<Box<tokio::time::Sleep>>);

impl Future for TokioSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

impl hyper::rt::Sleep for TokioSleep {}

impl hyper::rt::Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(TokioSleep(Box::pin(tokio::time::sleep(duration))))
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(TokioSleep(Box::pin(tokio::time::sleep_until(
            tokio::time::Instant::from_std(deadline),
        ))))
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn hyper::rt::Sleep>>, new_deadline: Instant) {
        *sleep = self.sleep_until(new_deadline);
    }
}

/// Safe `#![forbid(unsafe_code)]` adapter bridging Tokio [`AsyncRead`] /
/// [`AsyncWrite`] streams to Hyper's [`hyper::rt::Read`] / [`hyper::rt::Write`].
#[derive(Debug)]
pub struct TokioIo<T> {
    inner: T,
}

impl<T> TokioIo<T> {
    #[must_use]
    pub const fn new(inner: T) -> Self {
        Self { inner }
    }

    #[must_use]
    pub const fn inner(&self) -> &T {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    #[must_use]
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for TokioIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TokioIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

impl<T: AsyncRead + Unpin> hyper::rt::Read for TokioIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let remaining = buf.remaining();
        if remaining == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut scratch = [0_u8; 8192];
        let limit = remaining.min(scratch.len());
        let mut read_buf = ReadBuf::new(&mut scratch[..limit]);
        match Pin::new(&mut self.get_mut().inner).poll_read(cx, &mut read_buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Ready(Ok(())) => {
                let filled = read_buf.filled();
                if !filled.is_empty() {
                    buf.put_slice(filled);
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> hyper::rt::Write for TokioIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

struct PrefixedIo<I> {
    prefix: [u8; 24],
    len: usize,
    pos: usize,
    inner: I,
}

impl<I: AsyncRead + Unpin> AsyncRead for PrefixedIo<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.pos < this.len {
            let available = &this.prefix[this.pos..this.len];
            let count = available.len().min(buf.remaining());
            buf.put_slice(&available[..count]);
            this.pos += count;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for PrefixedIo<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// HTTP/1.1 settings mirror on [`Builder`].
pub struct Http1Builder<'a, E> {
    parent: &'a mut Builder<E>,
}

impl<E> Http1Builder<'_, E> {
    pub fn timer(&mut self, _timer: TokioTimer) -> &mut Self {
        self
    }

    pub fn header_read_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.parent.header_read_timeout = Some(timeout);
        self
    }
}

/// HTTP/1.1 + HTTP/2 auto-detecting server connection builder.
#[derive(Clone, Debug)]
pub struct Builder<E = TokioExecutor> {
    executor: E,
    header_read_timeout: Option<Duration>,
}

impl<E> Builder<E> {
    #[must_use]
    pub const fn new(executor: E) -> Self {
        Self {
            executor,
            header_read_timeout: None,
        }
    }

    pub fn http1(&mut self) -> Http1Builder<'_, E> {
        Http1Builder { parent: self }
    }
}

async fn sniff_h2_preface<I: AsyncRead + Unpin>(
    io: &mut I,
    prefix: &mut [u8; 24],
) -> io::Result<usize> {
    let mut len = 0;
    while len < H2_PREFACE.len() && prefix[..len] == H2_PREFACE[..len] {
        let n = io.read(&mut prefix[len..]).await?;
        if n == 0 {
            break;
        }
        len += n;
    }
    Ok(len)
}

impl Builder<TokioExecutor> {
    pub async fn serve_connection<I, S, B>(
        &self,
        mut io: I,
        service: S,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        I: AsyncRead + AsyncWrite + Unpin + 'static,
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Future: Send + 'static,
        B: hyper::body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let mut prefix = [0_u8; 24];
        let len = match self.header_read_timeout {
            Some(timeout) => {
                match tokio::time::timeout(timeout, sniff_h2_preface(&mut io, &mut prefix)).await {
                    Ok(res) => res?,
                    Err(elapsed) => return Err(Box::new(elapsed)),
                }
            }
            None => sniff_h2_preface(&mut io, &mut prefix).await?,
        };
        let is_h2 = len == H2_PREFACE.len() && &prefix == H2_PREFACE;
        let replayed = TokioIo::new(PrefixedIo {
            prefix,
            len,
            pos: 0,
            inner: io,
        });
        if is_h2 {
            let mut h2 = hyper::server::conn::http2::Builder::new(self.executor);
            h2.timer(TokioTimer::new());
            h2.serve_connection(replayed, service)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        } else {
            let mut h1 = hyper::server::conn::http1::Builder::new();
            h1.timer(TokioTimer::new());
            if let Some(timeout) = self.header_read_timeout {
                h1.header_read_timeout(timeout);
            }
            h1.serve_connection(replayed, service)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        }
    }
}

/// Connection builder with the header-read bound applied.
#[must_use]
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
    use crate::http_body::Full;
    use hyper::body::Bytes;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use std::convert::Infallible;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn header_read_timeout_is_thirty_seconds_and_builder_initializes() {
        assert_eq!(HEADER_READ_TIMEOUT, Duration::from_secs(30));
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

    #[test]
    fn header_read_timeout_micros_matches_thirty_million() {
        assert_eq!(HEADER_READ_TIMEOUT.as_micros(), 30_000_000);
        assert_eq!(HEADER_READ_TIMEOUT.as_nanos(), 30_000_000_000);
    }

    #[tokio::test]
    async fn auto_builder_serves_both_http1_and_http2_prior_knowledge() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let svc = service_fn(|req: Request<hyper::body::Incoming>| async move {
                        let ver = format!("{:?}", req.version());
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ver))))
                    });
                    connection_builder()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await
                        .unwrap();
                });
            }
        });

        // 1. HTTP/1.1 request
        let mut h1_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        h1_stream
            .write_all(b"GET /h1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut h1_reply = String::new();
        h1_stream.read_to_string(&mut h1_reply).await.unwrap();
        assert!(h1_reply.starts_with("HTTP/1.1 200 OK"), "{h1_reply}");
        assert!(h1_reply.ends_with("HTTP/1.1"), "{h1_reply}");

        // 2. HTTP/2 prior-knowledge request using raw RFC 9113 frames so
        // `susi-vendor-hyper` only enables `server` (`http1` + `http2`) and
        // never pulls `hyper/client` (`want` + `try-lock`).
        let mut h2_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let hpack_get_h2: &[u8] = &[
            0x82, // :method = GET (RFC 7541 static index 2)
            0x86, // :scheme = http (RFC 7541 static index 6)
            0x41, 0x09, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', // :authority
            0x44, 0x03, b'/', b'h', b'2', // :path = /h2
        ];
        let mut client_wire = Vec::new();
        client_wire.extend_from_slice(H2_PREFACE);
        // Empty client SETTINGS frame (type = 0x04, flags = 0x00, stream = 0).
        client_wire.extend_from_slice(&[0, 0, 0, 0x04, 0x00, 0, 0, 0, 0]);
        // HEADERS frame on stream 1 with END_STREAM | END_HEADERS (0x05).
        let hlen = hpack_get_h2.len() as u32;
        client_wire.extend_from_slice(&[
            ((hlen >> 16) & 0xff) as u8,
            ((hlen >> 8) & 0xff) as u8,
            (hlen & 0xff) as u8,
            0x01,
            0x05,
            0,
            0,
            0,
            1,
        ]);
        client_wire.extend_from_slice(hpack_get_h2);
        h2_stream.write_all(&client_wire).await.unwrap();

        let mut got_status_200 = false;
        let mut body = Vec::new();
        loop {
            let mut hdr = [0u8; 9];
            h2_stream.read_exact(&mut hdr).await.unwrap();
            let len = ((hdr[0] as usize) << 16) | ((hdr[1] as usize) << 8) | (hdr[2] as usize);
            let frame_type = hdr[3];
            let flags = hdr[4];
            let stream_id = u32::from_be_bytes([hdr[5] & 0x7f, hdr[6], hdr[7], hdr[8]]);
            let mut payload = vec![0u8; len];
            if len > 0 {
                h2_stream.read_exact(&mut payload).await.unwrap();
            }
            match frame_type {
                0x04 if (flags & 0x01) == 0 => {
                    // Acknowledge server SETTINGS.
                    h2_stream
                        .write_all(&[0, 0, 0, 0x04, 0x01, 0, 0, 0, 0])
                        .await
                        .unwrap();
                }
                0x01 if stream_id == 1 => {
                    // RFC 7541 static index 8 (`0x88`) encodes `:status: 200`.
                    assert_eq!(payload.first().copied(), Some(0x88));
                    got_status_200 = true;
                    if (flags & 0x01) != 0 {
                        break;
                    }
                }
                0x00 if stream_id == 1 => {
                    body.extend_from_slice(&payload);
                    if (flags & 0x01) != 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(got_status_200);
        assert_eq!(&body[..], b"HTTP/2.0");
        // Graceful GOAWAY (type = 0x07, stream = 0, last_stream_id = 0, NO_ERROR = 0).
        let _ = h2_stream
            .write_all(&[0, 0, 8, 0x07, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .await;
        let _ = h2_stream.shutdown().await;

        server.await.unwrap();
    }

    #[tokio::test]
    async fn auto_builder_enforces_header_read_timeout_on_stalled_preface() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut builder = Builder::new(TokioExecutor::new());
            builder
                .http1()
                .header_read_timeout(Duration::from_millis(60));
            let svc = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
            });
            builder.serve_connection(TokioIo::new(stream), svc).await
        });

        let mut slow_client = tokio::net::TcpStream::connect(addr).await.unwrap();
        slow_client.write_all(b"PRI ").await.unwrap();
        let outcome = server.await.unwrap();
        assert!(outcome.is_err(), "stalled preface must time out");
    }
}
