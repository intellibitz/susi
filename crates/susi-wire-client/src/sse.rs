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
mod tests {
    use super::*;

    fn events(raw: &[u8]) -> Vec<SseEvent> {
        SseReader::new(raw).collect::<io::Result<_>>().unwrap()
    }

    #[test]
    fn splits_on_blank_lines_and_keeps_fields() {
        let got = events(b"event: message\nid: 7\ndata: one\n\ndata: two\n\n");
        assert_eq!(
            got,
            vec![
                SseEvent {
                    event: Some("message".into()),
                    id: Some("7".into()),
                    data: "one".into()
                },
                SseEvent {
                    event: None,
                    id: None,
                    data: "two".into()
                },
            ]
        );
    }

    #[test]
    fn joins_multiline_data_and_tolerates_crlf() {
        let got = events(b"data: {\"a\":\r\ndata: 1}\r\n\r\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, "{\"a\":\n1}");
    }

    #[test]
    fn ignores_comments_retry_and_blocks_without_data() {
        let got = events(b": keep-alive\n\nretry: 3000\nid: 0\n\nevent: x\n: note\ndata:tight\n\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event.as_deref(), Some("x"));
        assert_eq!(got[0].data, "tight");
        assert_eq!(got[0].id, None);
    }

    /// GMCP's priming event carries an empty `data:` line; it is an event
    /// with empty data, which callers skip as not JSON.
    #[test]
    fn empty_data_line_is_an_event_with_empty_data() {
        let got = events(b"data: \nid: 0\nretry: 3000\n\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, "");
        assert_eq!(got[0].id.as_deref(), Some("0"));
    }

    #[test]
    fn final_event_without_trailing_blank_line_is_dispatched() {
        let got = events(b"data: a\n\ndata: last");
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].data, "last");
        assert!(events(b"").is_empty());
        assert!(events(b": only a comment\n").is_empty());
    }

    #[test]
    fn invalid_utf8_is_an_error_and_ends_the_stream() {
        let mut reader = SseReader::new(&b"data: \xff\n\ndata: ok\n\n"[..]);
        assert!(reader.next().unwrap().is_err());
        assert!(reader.next().is_none());
    }
}
