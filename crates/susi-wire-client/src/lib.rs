//! Std-only client for the local daemon's loopback socket: HTTP/1.1 with a
//! streaming body reader, Server-Sent Events, and MCP Streamable HTTP
//! sessions. It depends on std and JSON only, so the thin `susi` client can
//! talk to the daemon without an async runtime or an HTTP stack.
//!
//! It is for the loopback daemon socket only: it presents the host bearer
//! and never signs requests or seals bodies. Peer calls that need either go
//! through `susi_core::mcp_client`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod http;
pub mod mcp;
pub mod sse;

#[cfg(test)]
mod tests {
    // Test-only assertions in inline unit tests.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::io::Cursor;

    #[test]
    fn http_response_header_lookup_is_case_insensitive_and_text_decodes_utf8() {
        let resp = http::Response {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: b"{\"ok\":true}".to_vec(),
        };
        assert_eq!(resp.header("Content-Type"), Some("application/json"));
        assert_eq!(resp.header("X-Missing"), None);
        assert_eq!(resp.text(), "{\"ok\":true}");
    }

    #[test]
    fn sse_reader_parses_single_and_multiline_data_events() {
        let stream = "event: message\nid: 7\ndata: line1\ndata: line2\n\n";
        let mut reader = sse::SseReader::new(Cursor::new(stream.as_bytes()));
        let ev = reader.next().unwrap().unwrap();
        assert_eq!(ev.event.as_deref(), Some("message"));
        assert_eq!(ev.id.as_deref(), Some("7"));
        assert_eq!(ev.data, "line1\nline2");
        assert!(reader.next().is_none());
    }

    #[test]
    fn mcp_protocol_version_and_wire_error_display_formatting_are_stable() {
        assert_eq!(mcp::PROTOCOL_VERSION, "2025-11-25");
        let err = mcp::WireError::Rpc(-32601, "Method not found".to_string());
        let rendered = err.to_string();
        assert!(rendered.contains("-32601"), "{rendered}");
        assert!(rendered.contains("Method not found"), "{rendered}");
    }

    #[test]
    fn sse_reader_ignores_comment_only_blocks_and_exposes_max_event_bytes() {
        assert_eq!(sse::MAX_EVENT_BYTES, 16 * 1024 * 1024);
        let stream = ": keepalive\n\ndata: payload\n\n";
        let events: Vec<_> = sse::SseReader::new(Cursor::new(stream.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "payload");
    }

    #[test]
    fn http_response_empty_body_decodes_to_empty_text_and_missing_header_is_none() {
        let resp = http::Response {
            status: 204,
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert_eq!(resp.status, 204);
        assert!(resp.text().is_empty());
        assert_eq!(resp.header("content-type"), None);
    }
}
