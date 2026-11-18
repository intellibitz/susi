use super::*;

/// A chunk reaches the client sealed while the stream is still open —
/// the old path collected the whole body first, which never completes
/// for a long-lived SSE stream.
#[tokio::test]
async fn sealed_stream_emits_chunks_before_the_body_ends() {
    /// A body that stays open until its sender drops.
    struct OpenBody(tokio::sync::mpsc::Receiver<Bytes>);
    impl hyper::body::Body for OpenBody {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>> {
            self.0
                .poll_recv(cx)
                .map(|chunk| chunk.map(|b| Ok(hyper::body::Frame::data(b))))
        }
    }
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut sealed = seal_each_chunk(OpenBody(rx).boxed(), |chunk| [b"S:", chunk].concat());
    tx.send(Bytes::from_static(b"data: 1\n\n")).await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), sealed.frame())
        .await
        .expect("sealed chunk withheld until end of stream")
        .unwrap()
        .unwrap();
    assert_eq!(
        first.into_data().unwrap(),
        Bytes::from_static(b"S:data: 1\n\n")
    );
    drop(tx);
    assert!(sealed.frame().await.is_none());
}

#[test]
fn sessions_outlive_the_longest_tool_call() {
    let keep_alive = session_manager().session_config.keep_alive;
    assert!(
        keep_alive.is_some_and(|idle| idle > std::time::Duration::from_secs(MAX_TOOL_CALL_SECS)),
        "{keep_alive:?}"
    );
}

#[test]
fn rejections_carry_rfc_challenge_headers() {
    let unauthorized = response(StatusCode::UNAUTHORIZED, "Unauthorized");
    assert_eq!(
        unauthorized.headers()[hyper::header::WWW_AUTHENTICATE],
        "Bearer realm=\"susi-gmcp\""
    );
    let limited = response(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded");
    assert_eq!(limited.headers()[hyper::header::RETRY_AFTER], "60");
    let missing = response(StatusCode::NOT_FOUND, "nope");
    assert!(!missing
        .headers()
        .contains_key(hyper::header::WWW_AUTHENTICATE));
}

#[test]
fn origin_forbidden_and_cors_headers_enforce_browser_origin_policy() {
    let mut headers = hyper::HeaderMap::new();
    assert!(!origin_forbidden(&headers, "*"));

    headers.insert(
        hyper::header::ORIGIN,
        HeaderValue::from_static("https://app.example.com"),
    );
    assert!(origin_forbidden(&headers, "*"));
    assert!(!origin_forbidden(&headers, "https://app.example.com"));
    assert!(origin_forbidden(&headers, "https://other.example.com"));

    let mut resp = json_response(StatusCode::OK, &serde_json::json!({"ok": true}));
    cors(&mut resp, "https://app.example.com");
    assert_eq!(
        resp.headers()["access-control-allow-origin"],
        "https://app.example.com"
    );
}

#[test]
fn origin_forbidden_enforces_null_origin_and_explicit_port_matching() {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(hyper::header::ORIGIN, HeaderValue::from_static("null"));
    assert!(!origin_forbidden(&headers, "null"));
    assert!(origin_forbidden(&headers, "https://app.example.com"));

    headers.insert(
        hyper::header::ORIGIN,
        HeaderValue::from_static("https://app.example.com:8443"),
    );
    assert!(!origin_forbidden(&headers, "https://app.example.com"));
    assert!(!origin_forbidden(&headers, "https://app.example.com:8443"));
    assert!(origin_forbidden(&headers, "https://app.example.com:9443"));
}
