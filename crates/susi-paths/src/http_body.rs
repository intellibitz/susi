//! Blocking HTTP/1.1 body framing shared by the loopback and peer clients.

use std::io::{self, BufRead, Read};

/// Cap on a response head and each chunk-size or trailer line.
pub const MAX_HEAD_BYTES: u64 = 64 * 1024;

/// Read a line without letting a missing newline grow memory beyond `limit`.
pub fn read_line_capped<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    limit: u64,
) -> io::Result<()> {
    reader.take(limit).read_until(b'\n', line)?;
    if !line.ends_with(b"\n") && line.len() as u64 >= limit {
        return Err(invalid("response line too long"));
    }
    Ok(())
}

/// Streaming body decoder. `PEER_ERRORS` retains the peer channel's historical
/// EOF kinds and terse chunk-size diagnostic; the default retains the loopback
/// client's diagnostics. Header validation and connection budgets stay upstream.
#[derive(Debug)]
pub struct Body<R, const PEER_ERRORS: bool = false> {
    reader: R,
    framing: Framing,
}

#[derive(Debug)]
enum Framing {
    Length(u64),
    Chunked { remaining: u64, done: bool },
    UntilClose,
}

impl<R: BufRead, const PEER_ERRORS: bool> Body<R, PEER_ERRORS> {
    /// A chunked body read from `reader` (the bytes after the head).
    pub fn chunked(reader: R) -> Self {
        Self {
            reader,
            framing: Framing::Chunked {
                remaining: 0,
                done: false,
            },
        }
    }

    /// A body of exactly `length` bytes; a premature EOF is an error.
    pub fn with_length(reader: R, length: u64) -> Self {
        Self {
            reader,
            framing: Framing::Length(length),
        }
    }

    /// A body delimited by the underlying reader's EOF.
    pub fn until_close(reader: R) -> Self {
        Self {
            reader,
            framing: Framing::UntilClose,
        }
    }

    fn chunk_size(&mut self) -> io::Result<u64> {
        let mut line = Vec::new();
        read_line_capped(&mut self.reader, &mut line, MAX_HEAD_BYTES)?;
        if !line.ends_with(b"\n") {
            return Err(invalid("truncated chunk size"));
        }
        let text = String::from_utf8_lossy(&line);
        let digits = text.split(';').next().unwrap_or_default().trim();
        u64::from_str_radix(digits, 16).map_err(|_| {
            if PEER_ERRORS {
                invalid("invalid chunk size")
            } else {
                invalid(&format!("invalid chunk size {:?}", text.trim_end()))
            }
        })
    }

    /// A server closing immediately after the `0` line is tolerated.
    fn skip_trailers(&mut self) -> io::Result<()> {
        loop {
            let mut line = Vec::new();
            read_line_capped(&mut self.reader, &mut line, MAX_HEAD_BYTES)?;
            if line.is_empty() || line == b"\r\n" || line == b"\n" {
                return Ok(());
            }
        }
    }
}

impl<R: BufRead, const PEER_ERRORS: bool> Read for Body<R, PEER_ERRORS> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.framing {
            Framing::UntilClose => self.reader.read(buf),
            Framing::Length(0) | Framing::Chunked { done: true, .. } => Ok(0),
            Framing::Length(remaining) => {
                let want = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
                let n = self.reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "body shorter than its Content-Length",
                    ));
                }
                self.framing = Framing::Length(remaining - n as u64);
                Ok(n)
            }
            Framing::Chunked { remaining, .. } => {
                let remaining = if remaining == 0 {
                    let size = self.chunk_size()?;
                    if size == 0 {
                        self.skip_trailers()?;
                        self.framing = Framing::Chunked {
                            remaining: 0,
                            done: true,
                        };
                        return Ok(0);
                    }
                    size
                } else {
                    remaining
                };
                let want = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
                let n = self.reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(
                        if PEER_ERRORS {
                            io::ErrorKind::UnexpectedEof
                        } else {
                            io::ErrorKind::InvalidData
                        },
                        "truncated chunk",
                    ));
                }
                let left = remaining - n as u64;
                if left == 0 {
                    let mut crlf = [0u8; 2];
                    match self.reader.read_exact(&mut crlf) {
                        Ok(()) if &crlf == b"\r\n" => {}
                        Ok(()) => return Err(invalid("missing chunk terminator")),
                        Err(e) if !PEER_ERRORS && e.kind() == io::ErrorKind::UnexpectedEof => {
                            return Err(invalid("missing chunk terminator"));
                        }
                        // Peer EOFs and every client's timeouts retain their diagnostics.
                        Err(e) => return Err(e),
                    }
                }
                self.framing = Framing::Chunked {
                    remaining: left,
                    done: false,
                };
                Ok(n)
            }
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
#[path = "tests/http_body.rs"]
mod tests;
