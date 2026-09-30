//! A2A client streaming and push notifications.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamEvent {
    Chunk(String),
    Push(String),
    Done,
}

/// Fold a stream of events into the final text + push notices.
#[must_use]
pub fn fold_stream(events: &[StreamEvent]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut pushes = Vec::new();
    for e in events {
        match e {
            StreamEvent::Chunk(c) => text.push_str(c),
            StreamEvent::Push(p) => pushes.push(p.clone()),
            StreamEvent::Done => {}
        }
    }
    (text, pushes)
}

#[cfg(test)]
mod a2a_streaming_client_tests {
    use super::*;

    #[test]
    fn a2a_streaming_client_folds_chunks_and_pushes() {
        let (text, pushes) = fold_stream(&[
            StreamEvent::Chunk("hel".into()),
            StreamEvent::Push("progress".into()),
            StreamEvent::Chunk("lo".into()),
            StreamEvent::Done,
        ]);
        assert_eq!(text, "hello");
        assert_eq!(pushes, vec!["progress".to_string()]);
    }
}
