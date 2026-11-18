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

    /// `open`: the dead line's terminator is still inbound (`FileTooLarge`).
    fn oversized(open: bool) -> io::Error {
        let kind = [io::ErrorKind::InvalidData, io::ErrorKind::FileTooLarge][usize::from(open)];
        io::Error::new(kind, "MCP message exceeds configured byte limit")
    }

    /// Consume `slice`, delivering valid lines to `out`. Any bytes read
    /// past a dead line's terminator move to `pending` for the next
    /// call; the bound error is returned once per call spent inside an
    /// oversized line.
    fn scan(&mut self, mut slice: &[u8], out: &mut ReadBuf<'_>) -> io::Result<()> {
        let initial = out.filled().len();
        while !slice.is_empty() {
            if self.dropping {
                return match slice.iter().position(|b| *b == b'\n') {
                    // The dead line's terminator is consumed too; what
                    // follows belongs to the next message.
                    Some(p) => {
                        self.dropping = false;
                        self.length = 0;
                        self.pending.extend_from_slice(&slice[p + 1..]);
                        Err(Self::oversized(false))
                    }
                    None => Err(Self::oversized(true)),
                };
            }
            let newline = slice.iter().position(|b| *b == b'\n');
            let seg_len = newline.unwrap_or(slice.len());
            if self.length.saturating_add(seg_len) > self.limit {
                // Keep prior valid lines already written in this chunk intact;
                // defer the oversized line to `pending` so the next poll raises
                // the error cleanly without BufReader discarding earlier bytes.
                if out.filled().len() > initial {
                    self.pending.extend_from_slice(slice);
                    return Ok(());
                }
                self.length = 0;
                if let Some(p) = newline {
                    self.pending.extend_from_slice(&slice[p + 1..]);
                } else {
                    self.dropping = true;
                }
                return Err(Self::oversized(newline.is_none()));
            }
            let Some(p) = newline else {
                self.length = self.length.saturating_add(slice.len());
                out.put_slice(slice);
                return Ok(());
            };
            self.length = 0;
            out.put_slice(&slice[..=p]);
            slice = &slice[p + 1..];
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
#[path = "tests/stdio_inline.rs"]
mod tests;
