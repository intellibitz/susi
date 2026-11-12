//! VC-203-010: the resource server surface, proven on the wire against the
//! same `GmcpService` both entry points serve. Each test registers its own
//! providers through the public registration API.
use crate::mcp::model::*;
use crate::protocol::{GmcpService, ResourceHandler, MAX_RESOURCE_BYTES};
use crate::protocol_tests::{discover, modern, recv, send, service, wire, Wire};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A provider answering every read with `contents`, counting its calls.
fn provider(contents: Value, calls: Arc<AtomicUsize>) -> ResourceHandler {
    Arc::new(move |_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        let contents = contents.clone();
        Box::pin(async move {
            let result: ReadResourceResult =
                serde_json::from_value(json!({ "contents": contents })).unwrap();
            Ok(result.into())
        })
    })
}

fn text(uri: &str, body: &str) -> Value {
    json!([{"uri": uri, "mimeType": "text/plain", "text": body}])
}

async fn read(wire: &mut Wire, id: i64, uri: &str) -> Value {
    send(wire, modern(id, "resources/read", json!({ "uri": uri }))).await;
    recv(wire).await
}

async fn session(service: &GmcpService) -> Wire {
    let mut wire = wire(service.clone()).await;
    discover(&mut wire).await;
    wire
}

#[tokio::test]
async fn malformed_and_traversal_uris_never_reach_a_provider() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    // A template that would answer anything proves refusal happens first.
    let template =
        serde_json::from_value(json!({"uriTemplate":"vault://{path}","name":"vault"})).unwrap();
    service.register_template(
        template,
        provider(text("vault://x", "secret"), calls.clone()),
    );
    let mut wire = session(&service).await;
    let long = format!("vault://{}", "a".repeat(2048));
    let hostile = [
        "",
        "not a uri",
        "relative/path",
        "vault://a b",
        "vault://a\tb",
        "vault://a/../b",
        "vault://a/..",
        "vault://../etc/passwd",
        "vault://a/%2e%2e/b",
        "vault://a/%2E%2e%2Fb",
        "vault://a/..%5cb",
        "vault://a\\..\\b",
        "vault://a?x=/../y",
        "file:///srv/data/../../etc/shadow",
        long.as_str(),
    ];
    for (index, uri) in hostile.iter().enumerate() {
        let answer = read(&mut wire, 10 + index as i64, uri).await;
        assert_eq!(answer["error"]["code"], -32602, "{uri:?}: {answer}");
        assert!(answer.get("result").is_none(), "{uri:?} leaked: {answer}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a refused URI must never reach a provider"
    );
    // The same session still serves a well-formed read, dots inside a
    // segment included: only a whole `..` segment is traversal.
    let good = read(&mut wire, 90, "vault://notes/v1..2/readme.md").await;
    assert_eq!(good["result"]["contents"][0]["text"], "secret", "{good}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_guard_decides_before_read_and_subscribe() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    for uri in ["open://doc", "sealed://doc"] {
        service.register_resource(
            Resource::new(uri, "doc"),
            provider(text(uri, "body"), calls.clone()),
        );
    }
    service.set_resource_guard(Arc::new(|uri| {
        if uri.starts_with("sealed://") {
            return Err(ErrorData::invalid_request("Resource access denied", None));
        }
        Ok(())
    }));
    let mut wire = session(&service).await;
    let denied = read(&mut wire, 2, "sealed://doc").await;
    assert_eq!(denied["error"]["code"], -32600, "{denied}");
    assert_eq!(denied["error"]["message"], "Resource access denied");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "denial precedes the provider"
    );
    send(
        &mut wire,
        modern(3, "resources/subscribe", json!({"uri":"sealed://doc"})),
    )
    .await;
    let denied = recv(&mut wire).await;
    assert!(
        denied.get("error").is_some_and(|e| !e.is_null()),
        "a denied resource cannot be subscribed: {denied}"
    );
    let allowed = read(&mut wire, 4, "open://doc").await;
    assert_eq!(allowed["result"]["contents"][0]["text"], "body");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn binary_text_and_multi_part_reads_are_sealed() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    let cases = [
        (
            "bin://ok",
            json!([{"uri":"bin://ok","mimeType":"application/octet-stream","blob":"AAECAwQ="}]),
        ),
        (
            "bin://bad",
            json!([{"uri":"bin://bad","mimeType":"application/octet-stream","blob":"not base64!"}]),
        ),
        (
            "bin://unpadded",
            json!([{"uri":"bin://unpadded","blob":"AAECAwQ"}]),
        ),
        (
            "mime://bad",
            json!([{"uri":"mime://bad","mimeType":"not a mime","text":"x"}]),
        ),
        ("empty://none", json!([])),
        (
            "multi://ok",
            json!([
                {"uri":"multi://ok#a","mimeType":"text/plain; charset=utf-8","text":"alpha"},
                {"uri":"multi://ok#b","mimeType":"image/png","blob":"iVBORw0KGgo="}
            ]),
        ),
    ];
    for (uri, contents) in &cases {
        service.register_resource(
            Resource::new(*uri, "case"),
            provider(contents.clone(), calls.clone()),
        );
    }
    let mut wire = session(&service).await;

    let ok = read(&mut wire, 2, "bin://ok").await;
    let part = &ok["result"]["contents"][0];
    assert_eq!(part["blob"], "AAECAwQ=", "binary is served as base64: {ok}");
    assert_eq!(part["mimeType"], "application/octet-stream");
    assert!(part.get("text").is_none(), "a blob carries no text: {ok}");
    assert_eq!(ok["result"]["resultType"], "complete");
    assert_eq!(ok["result"]["ttlMs"], 0, "reads carry a cache lifetime");
    assert_eq!(
        ok["result"]["cacheScope"], "private",
        "reads carry a cache scope"
    );

    for (id, uri) in [
        (3, "bin://bad"),
        (4, "bin://unpadded"),
        (5, "mime://bad"),
        (6, "empty://none"),
    ] {
        let answer = read(&mut wire, id, uri).await;
        assert_eq!(answer["error"]["code"], -32603, "{uri}: {answer}");
        assert!(
            answer.get("result").is_none(),
            "{uri} must not be served: {answer}"
        );
    }

    let multi = read(&mut wire, 7, "multi://ok").await;
    let parts = multi["result"]["contents"].as_array().unwrap();
    assert_eq!(parts.len(), 2, "{multi}");
    assert_eq!(parts[0]["text"], "alpha");
    assert_eq!(parts[1]["blob"], "iVBORw0KGgo=");
}

#[tokio::test]
async fn reads_are_bounded_and_a_missing_resource_is_never_empty() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    let big = "x".repeat(1024 * 1024);
    service.register_resource(
        Resource::new("big://fits", "fits"),
        provider(text("big://fits", &big), calls.clone()),
    );
    let over = "y".repeat(MAX_RESOURCE_BYTES / 2 + 1);
    service.register_resource(
        Resource::new("big://over", "over"),
        provider(
            json!([
                {"uri":"big://over#1","text":over},
                {"uri":"big://over#2","text":over}
            ]),
            calls.clone(),
        ),
    );
    let (client, server) = tokio::io::duplex(4 * 1024 * 1024);
    let serving = service.clone();
    tokio::spawn(async move {
        use crate::mcp::ServiceExt;
        let running = serving.serve(server).await.unwrap();
        running.waiting().await.unwrap();
    });
    let mut wire = tokio::io::BufReader::new(client);
    discover(&mut wire).await;

    let fits = read(&mut wire, 2, "big://fits").await;
    assert_eq!(
        fits["result"]["contents"][0]["text"].as_str().map(str::len),
        Some(big.len()),
        "a large resource inside the bound is served whole"
    );
    let over = read(&mut wire, 3, "big://over").await;
    assert_eq!(
        over["error"]["code"], -32603,
        "an oversized read is refused"
    );
    assert!(
        over.get("result").is_none(),
        "an oversized read is never truncated into a result"
    );

    let missing = read(&mut wire, 4, "big://absent").await;
    assert!(
        missing.get("error").is_some_and(|e| !e.is_null()),
        "{missing}"
    );
    assert!(
        missing.get("result").is_none(),
        "a missing resource is an error, never an empty contents array: {missing}"
    );
    assert_eq!(missing["error"]["data"]["uri"], "big://absent");
    // The session survives every refusal.
    let again = read(&mut wire, 5, "big://fits").await;
    assert!(again.get("error").is_none());
}

#[tokio::test]
async fn listing_pages_cover_every_resource_once_and_detect_a_changed_catalog() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    let total = 257;
    for index in 0..total {
        let uri = format!("page://item/{index:04}");
        service.register_resource(
            Resource::new(uri.clone(), format!("item {index}")).with_mime_type("text/plain"),
            provider(text(&uri, "item"), calls.clone()),
        );
    }
    let mut wire = session(&service).await;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    let mut mid_cursor = None;
    loop {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        send(&mut wire, modern(10 + pages, "resources/list", params)).await;
        let page = recv(&mut wire).await;
        assert!(page.get("error").is_none(), "{page}");
        assert_eq!(page["result"]["cacheScope"], "private");
        for resource in page["result"]["resources"].as_array().unwrap() {
            assert_eq!(resource["mimeType"], "text/plain");
            seen.push(resource["uri"].as_str().unwrap().to_string());
        }
        pages += 1;
        cursor = page["result"]["nextCursor"].as_str().map(str::to_string);
        if pages == 1 {
            mid_cursor = cursor.clone();
        }
        if cursor.is_none() {
            break;
        }
        assert!(pages < 100, "pagination must terminate");
    }
    assert!(pages > 1, "a large catalog must be paged, got {pages} page");
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), total, "every resource appears exactly once");
    assert_eq!(seen, sorted, "listing order is deterministic");

    // The catalog changes between pages: an old cursor is refused rather
    // than silently skipping or repeating entries.
    service.register_resource(
        Resource::new("page://item/late", "late"),
        provider(text("page://item/late", "late"), calls.clone()),
    );
    let stale = mid_cursor.expect("a paged listing hands out a cursor");
    send(
        &mut wire,
        modern(200, "resources/list", json!({ "cursor": stale })),
    )
    .await;
    let refused = recv(&mut wire).await;
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    for (id, cursor) in [(201, "garbage"), (202, "1:0"), (203, "1:999999")] {
        send(
            &mut wire,
            modern(id, "resources/list", json!({ "cursor": cursor })),
        )
        .await;
        let refused = recv(&mut wire).await;
        assert_eq!(refused["error"]["code"], -32602, "{cursor}: {refused}");
    }
}

#[tokio::test]
async fn a_resource_replaced_during_reads_is_always_served_whole() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    let register = |service: &GmcpService, version: usize, calls: Arc<AtomicUsize>| {
        service.register_resource(
            Resource::new("race://doc", "doc"),
            provider(text("race://doc", &format!("version-{version}")), calls),
        );
    };
    register(&service, 0, calls.clone());
    let mut wire = session(&service).await;
    let writer = {
        let service = service.clone();
        let calls = calls.clone();
        tokio::spawn(async move {
            for version in 1..=50 {
                register(&service, version, calls.clone());
                tokio::task::yield_now().await;
            }
        })
    };
    let mut last = 0;
    for id in 0..100 {
        let answer = read(&mut wire, 100 + id, "race://doc").await;
        let text = answer["result"]["contents"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("a racing read must still be whole: {answer}"));
        let version: usize = text
            .strip_prefix("version-")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("torn content: {text}"));
        assert!(version >= last, "a read must never go back in time");
        last = version;
    }
    writer.await.unwrap();
    let settled = read(&mut wire, 999, "race://doc").await;
    assert_eq!(settled["result"]["contents"][0]["text"], "version-50");
}

#[tokio::test]
async fn a_cancelled_read_stops_its_provider_and_the_session_continues() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let service = service();
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let (started_flag, dropped_flag) = (started.clone(), dropped.clone());
    service.register_resource(
        Resource::new("slow://doc", "slow"),
        Arc::new(move |_, _| {
            let (started, dropped) = (started_flag.clone(), dropped_flag.clone());
            Box::pin(async move {
                let _guard = Dropped(dropped);
                started.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Err(ErrorData::internal_error("the slow read finished", None))
            })
        }),
    );
    service.register_resource(
        Resource::new("fast://doc", "fast"),
        provider(text("fast://doc", "quick"), Arc::new(AtomicUsize::new(0))),
    );
    let mut wire = session(&service).await;
    send(
        &mut wire,
        modern(2, "resources/read", json!({"uri":"slow://doc"})),
    )
    .await;
    for _ in 0..500 {
        if started.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        started.load(Ordering::SeqCst),
        "the slow provider must have started"
    );
    send(
        &mut wire,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}),
    )
    .await;
    for _ in 0..500 {
        if dropped.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        dropped.load(Ordering::SeqCst),
        "cancellation must stop the provider instead of leaving it running"
    );
    // The session answers the next read promptly; any answer to the
    // cancelled request is never a result.
    send(
        &mut wire,
        modern(3, "resources/read", json!({"uri":"fast://doc"})),
    )
    .await;
    loop {
        let answer = recv(&mut wire).await;
        if answer["id"] == 2 {
            assert!(
                answer.get("result").is_none(),
                "a cancelled read has no result: {answer}"
            );
            continue;
        }
        assert_eq!(answer["id"], 3, "{answer}");
        assert_eq!(answer["result"]["contents"][0]["text"], "quick");
        break;
    }
}

#[tokio::test]
async fn updates_and_list_changes_reach_only_what_was_subscribed() {
    let service = service();
    let calls = Arc::new(AtomicUsize::new(0));
    for uri in ["watch://a", "watch://b"] {
        service.register_resource(
            Resource::new(uri, "watched"),
            provider(text(uri, "v1"), calls.clone()),
        );
    }
    let mut wire = session(&service).await;
    send(
        &mut wire,
        modern(
            2,
            "subscriptions/listen",
            json!({"notifications":{"resourceSubscriptions":["watch://a"],"resourcesListChanged":true}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["method"],
        "notifications/subscriptions/acknowledged"
    );
    // An update to an unsubscribed resource is not delivered; the next
    // delivered notification is the subscribed one.
    service.resource_updated("watch://b".into());
    service.resource_updated("watch://a".into());
    let update = recv(&mut wire).await;
    assert_eq!(update["method"], "notifications/resources/updated");
    assert_eq!(update["params"]["uri"], "watch://a", "{update}");
    // Replacing a resource announces both the list change and the update.
    service.register_resource(
        Resource::new("watch://a", "watched"),
        provider(text("watch://a", "v2"), calls.clone()),
    );
    let mut methods = Vec::new();
    for _ in 0..2 {
        let note = recv(&mut wire).await;
        if note["method"] == "notifications/resources/updated" {
            assert_eq!(note["params"]["uri"], "watch://a");
        }
        methods.push(note["method"].as_str().unwrap().to_string());
    }
    methods.sort();
    assert_eq!(
        methods,
        [
            "notifications/resources/list_changed",
            "notifications/resources/updated"
        ]
    );
    let fresh = read(&mut wire, 3, "watch://a").await;
    assert_eq!(fresh["result"]["contents"][0]["text"], "v2");
}
