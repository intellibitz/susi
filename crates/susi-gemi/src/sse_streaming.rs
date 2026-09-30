//! Unified SSE streaming with cancellation across vendors.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SseEvent {
    Data(String),
    Done,
    Cancelled,
}

/// Fold SSE frames; stop early when cancelled.
#[must_use]
pub fn fold_sse(events: &[SseEvent]) -> (String, bool) {
    let mut out = String::new();
    for e in events {
        match e {
            SseEvent::Data(s) => out.push_str(s),
            SseEvent::Done => break,
            SseEvent::Cancelled => return (out, true),
        }
    }
    (out, false)
}

#[cfg(test)]
mod sse_streaming_tests {
    use super::*;

    #[test]
    fn sse_streaming_supports_cancellation() {
        let (t, cancelled) = fold_sse(&[
            SseEvent::Data("hi".into()),
            SseEvent::Cancelled,
            SseEvent::Data("ignored".into()),
        ]);
        assert_eq!(t, "hi");
        assert!(cancelled);
    }
}
