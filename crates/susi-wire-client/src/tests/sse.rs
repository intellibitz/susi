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
