//! Enforce the configured message limit before the SDK buffers a complete line.
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

/// Per-line byte bound for the stdio transport. Once a line exceeds the
/// limit the reader enters `dropping`: every byte is consumed and
/// discarded until the line's terminating `\n`, after which normal
/// delivery resumes — the following message is never corrupted by the
/// oversized line's tail. Bytes read past the dead line's terminator in
/// the same chunk are stashed in `pending` and scanned on the next call,
/// so a read chunk that straddles the boundary cannot lose the next
/// message's head. Each call made while inside the dead line returns the
/// bound error once; the caller decides whether to keep reading (the
/// gate does, answering `-32600`) or end the session.
pub struct BoundedLines<R> {
    reader: R,
    limit: usize,
    /// Bytes on the current line so far (the newline itself is not
    /// counted).
    length: usize,
    /// Inside an oversized line's discarded tail.
    dropping: bool,
    /// Read-ahead bytes saved across an oversized-line trip.
    pending: Vec<u8>,
}
impl<R> BoundedLines<R> {
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            limit,
            length: 0,
            dropping: false,
            pending: Vec::new(),
        }
    }

    fn oversized() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP message exceeds configured byte limit",
        )
    }

    /// Consume `slice`, delivering valid lines to `out`. Any bytes read
    /// past a dead line's terminator move to `pending` for the next
    /// call; the bound error is returned once per call spent inside an
    /// oversized line.
    fn scan(&mut self, mut slice: &[u8], out: &mut ReadBuf<'_>) -> io::Result<()> {
        while !slice.is_empty() {
            if self.dropping {
                return match slice.iter().position(|b| *b == b'\n') {
                    // The dead line's terminator is consumed too; what
                    // follows belongs to the next message.
                    Some(p) => {
                        self.dropping = false;
                        self.length = 0;
                        self.pending.extend_from_slice(&slice[p + 1..]);
                        Err(Self::oversized())
                    }
                    None => Err(Self::oversized()),
                };
            }
            match slice.iter().position(|b| *b == b'\n') {
                Some(p) => {
                    if self.length.saturating_add(p) > self.limit {
                        // Oversized — but the terminator is already here,
                        // so the stream is past the dead line. Stash the
                        // tail; the next call resumes cleanly.
                        self.length = 0;
                        self.pending.extend_from_slice(&slice[p + 1..]);
                        return Err(Self::oversized());
                    }
                    self.length = 0;
                    out.put_slice(&slice[..=p]);
                    slice = &slice[p + 1..];
                }
                None => {
                    if self.length.saturating_add(slice.len()) > self.limit {
                        // The terminator is still inbound — drop every
                        // byte until it arrives.
                        self.dropping = true;
                        self.length = 0;
                        return Err(Self::oversized());
                    }
                    self.length = self.length.saturating_add(slice.len());
                    out.put_slice(slice);
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
impl<R: AsyncRead + Unpin> AsyncRead for BoundedLines<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // Bytes saved past a dead line's terminator deliver first,
        // through the same scan — a stashed line is still bounded.
        if !this.pending.is_empty() {
            let saved = std::mem::take(&mut this.pending);
            let n = saved.len().min(out.remaining());
            let result = this.scan(&saved[..n], out);
            this.pending.extend_from_slice(&saved[n..]);
            return Poll::Ready(result);
        }
        let mut buffer = [0u8; 8192];
        let capacity = out.remaining().min(buffer.len());
        let mut input = ReadBuf::new(&mut buffer[..capacity]);
        match Pin::new(&mut this.reader).poll_read(cx, &mut input) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => Poll::Ready(this.scan(input.filled(), out)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    #[tokio::test]
    async fn limits_individual_lines_including_fragmented_reads() {
        let mut reader = BoundedLines::new(&b"123\n123\n"[..], 3);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, b"123\n123\n");
        let mut reader = BoundedLines::new(&b"12345\n"[..], 3);
        let mut first = [0; 2];
        reader.read_exact(&mut first).await.unwrap();
        assert!(reader.read_exact(&mut first).await.is_err());
    }

    #[tokio::test]
    async fn recovers_after_the_oversized_line() {
        // Trip and terminator in the same chunk: the next line survives.
        let mut reader = BoundedLines::new(&b"12345\nok\n"[..], 3);
        let mut buf = [0u8; 16];
        assert!(reader.read(&mut buf).await.is_err());
        let mut line = String::new();
        reader.read_to_string(&mut line).await.unwrap();
        assert_eq!(line, "ok\n");

        // Trip and terminator in different chunks: same recovery.
        let mut reader = BoundedLines::new(&b"1234567890\nok"[..], 3);
        let mut recovered = Vec::new();
        loop {
            match reader.read(&mut buf).await {
                Err(_) => {}
                Ok(0) => break,
                Ok(n) => recovered.extend_from_slice(&buf[..n]),
            }
        }
        assert_eq!(recovered, b"ok");

        // A stashed line is still bounded.
        let mut reader = BoundedLines::new(&b"12345\n6789\nok\n"[..], 3);
        let mut recovered = Vec::new();
        let mut errors = 0;
        loop {
            match reader.read(&mut buf).await {
                Err(_) => errors += 1,
                Ok(0) => break,
                Ok(n) => recovered.extend_from_slice(&buf[..n]),
            }
            if errors > 8 {
                panic!("recovery must terminate");
            }
        }
        assert!(errors >= 2, "both oversized lines must error");
        assert_eq!(recovered, b"ok\n");
    }
}
