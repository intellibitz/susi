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

    #[test]
    fn fold_sse_stops_at_done_and_ignores_trailing_frames() {
        let (text, cancelled) = fold_sse(&[
            SseEvent::Data("alpha-".into()),
            SseEvent::Data("beta".into()),
            SseEvent::Done,
            SseEvent::Data("-trailing".into()),
            SseEvent::Cancelled,
        ]);
        assert_eq!(text, "alpha-beta");
        assert!(!cancelled);
    }

    #[test]
    fn fold_sse_handles_empty_and_unterminated_data_streams() {
        let (empty_text, empty_cancelled) = fold_sse(&[]);
        assert!(empty_text.is_empty());
        assert!(!empty_cancelled);

        let (joined, cancelled) = fold_sse(&[
            SseEvent::Data("one ".into()),
            SseEvent::Data("two ".into()),
            SseEvent::Data("three".into()),
        ]);
        assert_eq!(joined, "one two three");
        assert!(!cancelled);
    }

    #[test]
    fn sse_event_variants_round_trip_through_serde_json() {
        for event in [
            SseEvent::Data("chunk".into()),
            SseEvent::Done,
            SseEvent::Cancelled,
        ] {
            let encoded = serde_json::to_string(&event).expect("serialize SseEvent");
            let decoded: SseEvent = serde_json::from_str(&encoded).expect("deserialize SseEvent");
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn fold_sse_immediate_done_or_cancel_returns_empty_output() {
        let (done_out, done_cancelled) =
            fold_sse(&[SseEvent::Done, SseEvent::Data("later".into())]);
        assert!(done_out.is_empty());
        assert!(!done_cancelled);

        let (cancel_out, cancel_cancelled) =
            fold_sse(&[SseEvent::Cancelled, SseEvent::Data("later".into())]);
        assert!(cancel_out.is_empty());
        assert!(cancel_cancelled);
    }
}
