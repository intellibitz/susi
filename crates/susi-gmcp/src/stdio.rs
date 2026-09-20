//! Enforce the configured message limit before the SDK buffers a complete line.
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};

pub struct BoundedLines<R> {
    reader: R,
    limit: usize,
    length: usize,
}
impl<R> BoundedLines<R> {
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            limit,
            length: 0,
        }
    }
}
impl<R: AsyncRead + Unpin> AsyncRead for BoundedLines<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut buffer = [0u8; 8192];
        let capacity = out.remaining().min(buffer.len());
        let mut input = ReadBuf::new(&mut buffer[..capacity]);
        match Pin::new(&mut this.reader).poll_read(cx, &mut input) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                for byte in input.filled() {
                    if *byte == b'\n' {
                        this.length = 0;
                    } else {
                        this.length = this.length.saturating_add(1);
                        if this.length > this.limit {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "MCP message exceeds configured byte limit",
                            )));
                        }
                    }
                }
                out.put_slice(input.filled());
                Poll::Ready(Ok(()))
            }
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
}
