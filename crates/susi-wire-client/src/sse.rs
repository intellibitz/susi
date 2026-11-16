//! Server-Sent Events reader (the `text/event-stream` format): one
//! [`SseEvent`] per blank-line-terminated block, read incrementally from
//! any `BufRead`, such as a streaming HTTP body.

use std::io::{self, BufRead, Read};

/// Upper bound on one event's bytes, so a stream that never sends a blank
/// line cannot grow memory without limit.
pub const MAX_EVENT_BYTES: u64 = 16 * 1024 * 1024;

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` field, when the block had one.
    pub event: Option<String>,
    /// The `id:` field, when the block had one.
    pub id: Option<String>,
    /// Every `data:` line of the block, joined with `\n`.
    pub data: String,
}

/// Iterator over the events of an SSE stream. Lines may end in LF or CRLF;
/// `:` comment lines and unknown fields (`retry:`, ...) are ignored; a block
/// with no `data:` line is not dispatched. A final block cut off by the end
/// of the stream (no trailing blank line) is still dispatched when it holds
/// data, so a body that simply ends after its last event is read whole.
#[derive(Debug)]
pub struct SseReader<R> {
    reader: R,
    finished: bool,
}

impl<R: BufRead> SseReader<R> {
    /// Read events from `reader`.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            finished: false,
        }
    }

    fn next_event(&mut self) -> io::Result<Option<SseEvent>> {
        let mut event = SseEvent::default();
        let mut has_data = false;
        let mut used: u64 = 0;
        loop {
            let mut raw = Vec::new();
            let budget = MAX_EVENT_BYTES.saturating_sub(used) + 1;
            let n = (&mut self.reader)
                .take(budget)
                .read_until(b'\n', &mut raw)?;
            used += n as u64;
            if used > MAX_EVENT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("SSE event exceeds {MAX_EVENT_BYTES} bytes"),
                ));
            }
            if n == 0 {
                self.finished = true;
                return Ok(has_data.then_some(event));
            }
            let line = std::str::from_utf8(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let line = line
                .strip_suffix('\n')
                .map_or(line, |l| l.strip_suffix('\r').unwrap_or(l));
            if line.is_empty() {
                if has_data {
                    return Ok(Some(event));
                }
                event = SseEvent::default();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    if has_data {
                        event.data.push('\n');
                    }
                    event.data.push_str(value);
                    has_data = true;
                }
                "event" => event.event = Some(value.to_string()),
                "id" if !value.contains('\0') => event.id = Some(value.to_string()),
                _ => {}
            }
        }
    }
}

impl<R: BufRead> Iterator for SseReader<R> {
    type Item = io::Result<SseEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.next_event() {
            Ok(Some(event)) => Some(Ok(event)),
            Ok(None) => None,
            Err(e) => {
                self.finished = true;
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/sse.rs"]
mod tests;
