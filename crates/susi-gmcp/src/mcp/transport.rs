//! Transports carry JSON-RPC messages between a session and its client.
//!
//! [`Transport::receive`] must be cancel-safe: the session polls it inside
//! a `select!` and drops it whenever another event wins, so a partially
//! read message has to survive on `self`.

use super::message::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

pub trait Transport: Send + 'static {
    /// Queue one message; the returned future completes once it is written.
    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = std::io::Result<()>> + Send + 'static;
    /// The next message, or `None` once the client's side is closed.
    fn receive(&mut self) -> impl Future<Output = Option<ClientJsonRpcMessage>> + Send;
    fn close(&mut self) -> impl Future<Output = std::io::Result<()>> + Send;
}

/// Anything a session can be served on. `A` only disambiguates the impls.
pub trait IntoTransport<A>: Send + 'static {
    type Transport: Transport;
    fn into_transport(self) -> Self::Transport;
}

pub enum TransportAdapterIdentity {}

impl<T: Transport> IntoTransport<TransportAdapterIdentity> for T {
    type Transport = T;
    fn into_transport(self) -> T {
        self
    }
}

pub enum TransportAdapterAsyncCombinedRW {}

impl<S> IntoTransport<TransportAdapterAsyncCombinedRW> for S
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    type Transport = AsyncRwTransport<tokio::io::ReadHalf<S>, tokio::io::WriteHalf<S>>;
    fn into_transport(self) -> Self::Transport {
        let (read, write) = tokio::io::split(self);
        AsyncRwTransport::new(read, write)
    }
}

/// Bound on one newline-delimited message on a raw byte stream.
pub const DEFAULT_MAX_LINE_LENGTH: usize = 16 * 1024 * 1024;

/// Newline-delimited JSON over a byte stream, without the GMCP envelope
/// gate: an unparseable line is ignored, a parseable line no message shape
/// fits is answered `-32600` with no id, an oversized line is discarded.
pub struct AsyncRwTransport<R, W> {
    read: BufReader<R>,
    line: Vec<u8>,
    discarding: bool,
    write: Arc<Mutex<Option<W>>>,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> AsyncRwTransport<R, W> {
    pub fn new(read: R, write: W) -> Self {
        Self {
            read: BufReader::new(read),
            line: Vec::new(),
            discarding: false,
            write: Arc::new(Mutex::new(Some(write))),
        }
    }

    /// Read one whole line into `self.line`; `Ok(false)` at end of stream.
    /// Cancel-safe: bytes consumed so far stay in `self.line`.
    async fn read_line(&mut self) -> std::io::Result<Option<bool>> {
        loop {
            let available = self.read.fill_buf().await?;
            if available.is_empty() {
                return Ok(None);
            }
            let newline = available.iter().position(|b| *b == b'\n');
            let take = newline.map_or(available.len(), |i| i + 1);
            let oversized =
                !self.discarding && self.line.len().saturating_add(take) > DEFAULT_MAX_LINE_LENGTH;
            if !self.discarding && !oversized {
                self.line.extend_from_slice(&available[..take]);
            }
            self.read.consume(take);
            if self.discarding {
                if newline.is_some() {
                    self.discarding = false;
                }
                continue;
            }
            if oversized {
                self.line.clear();
                self.discarding = newline.is_none();
                return Ok(Some(false));
            }
            if newline.is_some() {
                return Ok(Some(true));
            }
        }
    }
}

async fn write_line<W: AsyncWrite + Unpin>(
    write: &Mutex<Option<W>>,
    bytes: &[u8],
) -> std::io::Result<()> {
    let mut guard = write.lock().await;
    let Some(w) = guard.as_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "Transport is closed",
        ));
    };
    w.write_all(bytes).await?;
    w.flush().await
}

impl<R, W> Transport for AsyncRwTransport<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = std::io::Result<()>> + Send + 'static {
        let write = self.write.clone();
        async move {
            let mut bytes = serde_json::to_vec(&item)?;
            bytes.push(b'\n');
            write_line(&write, &bytes).await
        }
    }

    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        loop {
            match self.read_line().await {
                Ok(Some(true)) => {}
                Ok(Some(false)) => continue,
                Ok(None) | Err(_) => return None,
            }
            let mut line = std::mem::take(&mut self.line);
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }
            let line = line
                .strip_prefix(b"\xEF\xBB\xBF".as_slice())
                .unwrap_or(&line);
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(message) = ClientJsonRpcMessage::from_value(value.clone()) {
                return Some(message);
            }
            if is_ignorable_notification(&value) {
                continue;
            }
            let answer = br#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid request"}}
"#;
            if write_line(&self.write, answer).await.is_err() {
                return None;
            }
        }
    }

    async fn close(&mut self) -> std::io::Result<()> {
        self.write.lock().await.take();
        Ok(())
    }
}

/// Methods a peer may legitimately name; anything else without an id is a
/// foreign notification (LSP and friends) that is ignored.
pub fn is_standard_method(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "ping"
            | "server/discover"
            | "prompts/get"
            | "prompts/list"
            | "resources/list"
            | "resources/read"
            | "resources/subscribe"
            | "resources/unsubscribe"
            | "resources/templates/list"
            | "subscriptions/listen"
            | "tools/call"
            | "tools/list"
            | "completion/complete"
            | "elicitation/create"
            | "logging/setLevel"
            | "roots/list"
            | "sampling/createMessage"
    ) || is_standard_notification(method)
}

pub fn is_standard_notification(method: &str) -> bool {
    matches!(
        method,
        "notifications/cancelled"
            | "notifications/initialized"
            | "notifications/message"
            | "notifications/progress"
            | "notifications/prompts/list_changed"
            | "notifications/resources/list_changed"
            | "notifications/resources/updated"
            | "notifications/roots/list_changed"
            | "notifications/subscriptions/acknowledged"
            | "notifications/tools/list_changed"
    )
}

fn is_ignorable_notification(value: &serde_json::Value) -> bool {
    let Some(method) = value.get("method").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let notification = value.get("id").is_none();
    (notification && !is_standard_method(method))
        || (method.starts_with("notifications/") && !is_standard_notification(method))
}
