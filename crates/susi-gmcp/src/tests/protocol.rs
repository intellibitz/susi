//! Wire-level tests exercise the same GMCP MCP service used by both entry points.
use crate::mcp::{model::*, ServiceExt};
use crate::protocol::GmcpService;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Serializes every test in this crate that replaces the process-global
/// `PlaneBus` handlers for `TOOLS_LIST` / `TOOLS_EXISTS` / `TOOLS_EXECUTE`.
/// `GmcpService::tools()` and `tools/call` read those topics, so two such tests
/// running in parallel each served the other's stub: `mission_run` became an
/// "Unknown tool" whenever the extension-pack router registered mid-test.
pub(crate) static TOOLS_PLANE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) type Wire = BufReader<DuplexStream>;
pub(crate) fn service() -> GmcpService {
    GmcpService::new(std::path::PathBuf::from("."))
}
pub(crate) async fn wire(service: GmcpService) -> Wire {
    let (client, server) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let running = service.serve(server).await.unwrap();
        running.waiting().await.unwrap();
    });
    BufReader::new(client)
}
pub(crate) async fn send(wire: &mut Wire, message: Value) {
    wire.get_mut()
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}
pub(crate) async fn recv(wire: &mut Wire) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), wire.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
pub(crate) fn modern(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":{}});
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}
pub(crate) async fn discover(wire: &mut Wire) {
    send(wire, modern(1, "server/discover", json!({}))).await;
    let response = recv(wire).await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["resultType"], "complete");
}
fn echo(service: &GmcpService) {
    let tool: Tool = serde_json::from_value(json!({"name":"test_echo", "inputSchema":{"type":"object"}, "outputSchema":{"type":"object"}})).unwrap();
    service
        .register_tool(
            tool,
            Arc::new(|request, _| {
                Box::pin(async move {
                    Ok(CallToolResult::structured(Value::Object(
                        request.arguments.unwrap_or_default(),
                    ))
                    .into())
                })
            }),
        )
        .unwrap();
}
#[tokio::test]
async fn current_discovery_tools_structured_output_and_errors() {
    let service = service();
    echo(&service);
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    // The built-in registry exceeds one page, so walk every cursor (bounded).
    // Cursors are revision-bound and parallel tests mutate the process-wide
    // tool registry, so a "Catalog changed" answer restarts the listing
    // without a cursor, exactly as the error instructs a client to.
    let mut tools = Vec::new();
    let mut params = json!({});
    let mut complete = false;
    for id in 100..150 {
        send(&mut wire, modern(id, "tools/list", params)).await;
        let listed = recv(&mut wire).await;
        if listed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("Catalog changed"))
        {
            tools.clear();
            params = json!({});
            continue;
        }
        assert!(listed["result"].get("ttlMs").is_some(), "{listed}");
        tools.extend(listed["result"]["tools"].as_array().unwrap().clone());
        match listed["result"]["nextCursor"].as_str() {
            Some(cursor) => params = json!({"cursor": cursor}),
            None => {
                complete = true;
                break;
            }
        }
    }
    assert!(complete, "tools/list must finish paging within the bound");
    assert!(tools
        .iter()
        .any(|t| t["name"] == "test_echo" && t["inputSchema"]["type"] == "object"));
    send(
        &mut wire,
        modern(
            3,
            "tools/call",
            json!({"name":"test_echo","arguments":{"answer":42}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["result"]["structuredContent"]["answer"],
        42
    );
    send(
        &mut wire,
        modern(4, "tools/call", json!({"name":"no_such_tool"})),
    )
    .await;
    assert_eq!(recv(&mut wire).await["error"]["code"], -32602);
    let mut unsupported = modern(5, "tools/list", json!({}));
    unsupported["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    send(&mut wire, unsupported).await;
    assert_eq!(recv(&mut wire).await["error"]["code"], -32022);
}
#[tokio::test]
async fn legacy_initialization_and_ping() {
    let mut wire = wire(service()).await;
    send(&mut wire, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
    let response = recv(&mut wire).await;
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    send(
        &mut wire,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    send(&mut wire, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
    let response = recv(&mut wire).await;
    assert_eq!(response["id"], 2);
    assert!(response["result"].get("resultType").is_none());
}
#[tokio::test]
async fn resources_prompts_completion_and_subscription() {
    let service = service();
    service.register_resource(Resource::new("test://document", "document"), Arc::new(|_,_| Box::pin(async {
        let result: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":"test://document","mimeType":"text/plain","text":"hello"}],"ttlMs":0,"cacheScope":"private"})).unwrap();
        Ok(result.into())
    })));
    let prompt = serde_json::from_value(
        json!({"name":"greet","arguments":[{"name":"name","required":true}]}),
    )
    .unwrap();
    service.register_prompt(prompt, Arc::new(|request,_| Box::pin(async move {
        let result: GetPromptResult = serde_json::from_value(json!({"messages":[{"role":"user","content":{"type":"text","text":request.arguments.unwrap()["name"]}}]})).unwrap();
        Ok(result.into())
    })));
    service.register_completion(
        "greet".into(),
        Arc::new(|_, _| {
            Box::pin(async {
                Ok(serde_json::from_value(
                    json!({"completion":{"values":["world"],"total":1,"hasMore":false}}),
                )
                .unwrap())
            })
        }),
    );
    let mut wire = wire(service.clone()).await;
    discover(&mut wire).await;
    send(
        &mut wire,
        modern(2, "resources/read", json!({"uri":"test://document"})),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["result"]["contents"][0]["text"],
        "hello"
    );
    send(
        &mut wire,
        modern(
            3,
            "prompts/get",
            json!({"name":"greet","arguments":{"name":"world"}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["result"]["messages"][0]["content"]["text"],
        "world"
    );
    send(&mut wire, modern(4, "prompts/get", json!({"name":"greet"}))).await;
    assert_eq!(recv(&mut wire).await["error"]["code"], -32602);
    send(&mut wire, modern(5, "completion/complete", json!({"ref":{"type":"ref/prompt","name":"greet"},"argument":{"name":"name","value":"w"}}))).await;
    assert_eq!(
        recv(&mut wire).await["result"]["completion"]["values"][0],
        "world"
    );
    send(
        &mut wire,
        modern(
            6,
            "subscriptions/listen",
            json!({"notifications":{"resourceSubscriptions":["test://document"]}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["method"],
        "notifications/subscriptions/acknowledged"
    );
    service.resource_updated("test://document".into());
    let notification = recv(&mut wire).await;
    assert_eq!(notification["method"], "notifications/resources/updated");
    assert_eq!(notification["params"]["uri"], "test://document");
    send(
        &mut wire,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":6}}),
    )
    .await;
}
#[tokio::test]
async fn task_poll_update_and_cooperative_cancellation() {
    let service = service();
    let task = service.tasks.spawn(Default::default(), |context| {
        Box::pin(async move {
            context.cancelled().await;
            Err(crate::mcp::task_manager::TaskExit::Cancelled)
        })
    });
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    let request = |id, method| {
        let mut request = modern(id, method, json!({"taskId":task.task_id}));
        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
            json!({"extensions":{"io.modelcontextprotocol/tasks":{}}});
        request
    };
    send(&mut wire, request(2, "tasks/get")).await;
    let response = recv(&mut wire).await;
    assert_eq!(response["result"]["status"], "working", "{response}");
    send(&mut wire, request(3, "tasks/cancel")).await;
    assert!(recv(&mut wire).await.get("error").is_none());
    for id in 4..20 {
        send(&mut wire, request(id, "tasks/get")).await;
        let response = recv(&mut wire).await;
        if response["result"]["status"] == "cancelled" {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("task never observed cancellation");
}

#[tokio::test]
async fn http_discovery_header_validation_and_origin_protection() {
    use hyper::body::Bytes;
    use susi_http_transport::http_body::{BodyExt, Full};
    let http = crate::server::http_service(service());
    let request = |method: &str, host: &str, origin: Option<&str>| {
        let mut builder = hyper::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", host)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", method);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder
            .body(Full::new(Bytes::from(
                modern(1, "server/discover", json!({})).to_string(),
            )))
            .unwrap()
    };
    let response = http
        .handle(request("server/discover", "localhost", None))
        .await;
    assert_eq!(response.status(), hyper::StatusCode::OK);
    assert!(response.headers().get("Mcp-Session-Id").is_none());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("susi-gmcp"));
    let response = http.handle(request("tools/list", "localhost", None)).await;
    assert_eq!(response.status(), hyper::StatusCode::BAD_REQUEST);
    let response = http
        .handle(request("server/discover", "attacker.invalid", None))
        .await;
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
    let response = http
        .handle(request(
            "server/discover",
            "localhost",
            Some("https://attacker.invalid"),
        ))
        .await;
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn elicitation_mrtr_preserves_input_responses() {
    let service = service();
    let definition =
        serde_json::from_value(json!({"name":"ask","inputSchema":{"type":"object"}})).unwrap();
    service.register_tool(definition, Arc::new(|request,_| Box::pin(async move {
        if let Some(responses) = request.input_responses {
            return Ok(CallToolResult::structured(responses["answer"].clone()).into());
        }
        let input: InputRequiredResult = serde_json::from_value(json!({
            "resultType":"input_required","inputRequests":{"answer":{"method":"elicitation/create","params":{
                "mode":"form","message":"Choose a name", "requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}
            }}}
        })).unwrap();
        Ok(input.into())
    }))).unwrap();
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    let mut request = modern(2, "tools/call", json!({"name":"ask"}));
    request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
        json!({"elicitation":{"form":{}}});
    send(&mut wire, request.clone()).await;
    let response = recv(&mut wire).await;
    assert_eq!(
        response["result"]["resultType"], "input_required",
        "{response}"
    );
    request["id"] = json!(3);
    request["params"]["inputResponses"] =
        json!({"answer":{"action":"accept","content":{"name":"Ada"}}});
    send(&mut wire, request).await;
    assert_eq!(
        recv(&mut wire).await["result"]["structuredContent"]["content"]["name"],
        "Ada"
    );
}

#[tokio::test]
async fn schema_validation_prevents_execution_and_invalid_output() {
    let service = service();
    let tool = serde_json::from_value(json!({"name":"validate", "inputSchema":{"type":"object","$defs":{"positive":{"type":"integer","minimum":1}},"properties":{"n":{"$ref":"#/$defs/positive"}},"required":["n"]},"outputSchema":{"type":"object","required":["answer"]}})).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    service
        .register_tool(
            tool,
            Arc::new(move |_, _| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(CallToolResult::structured(json!({"wrong":42})).into()) })
            }),
        )
        .unwrap();
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    send(
        &mut wire,
        modern(
            2,
            "tools/call",
            json!({"name":"validate","arguments":{"n":0}}),
        ),
    )
    .await;
    assert_eq!(recv(&mut wire).await["result"]["isError"], true);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    send(
        &mut wire,
        modern(
            3,
            "tools/call",
            json!({"name":"validate","arguments":{"n":1}}),
        ),
    )
    .await;
    assert_eq!(recv(&mut wire).await["result"]["isError"], true);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn task_tool_returns_handle_and_accepts_midflight_input() {
    let service = service();
    let tool = serde_json::from_value(json!({"name":"task_input","inputSchema":{"type":"object"}}))
        .unwrap();
    service.register_task_tool(tool, Arc::new(|_,context| Box::pin(async move {
        let request: InputRequest = serde_json::from_value(json!({"method":"elicitation/create","params":{"mode":"form","message":"Name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}})).unwrap();
        let input = context.request_input("name",request).await?;
        Ok(CallToolResult::structured(input))
    }))).unwrap();
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    let request = |id, method, params| {
        let mut request = modern(id, method, params);
        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
            json!({"extensions":{"io.modelcontextprotocol/tasks":{}},"elicitation":{"form":{}}});
        request
    };
    send(
        &mut wire,
        request(2, "tools/call", json!({"name":"task_input"})),
    )
    .await;
    let result = recv(&mut wire).await;
    assert_eq!(result["result"]["resultType"], "task", "{result}");
    let task_id = result["result"]["taskId"].clone();
    let mut ready = false;
    for id in 3..20 {
        send(
            &mut wire,
            request(id, "tasks/get", json!({"taskId":task_id})),
        )
        .await;
        let result = recv(&mut wire).await;
        if result["result"]["inputRequests"]["name"].is_object() {
            ready = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(ready);
    send(&mut wire, request(20,"tasks/update",json!({"taskId":task_id,"inputResponses":{"name":{"action":"accept","content":{"name":"Ada"}}}}))).await;
    assert!(recv(&mut wire).await.get("error").is_none());
    for id in 21..40 {
        send(
            &mut wire,
            request(id, "tasks/get", json!({"taskId":task_id})),
        )
        .await;
        let result = recv(&mut wire).await;
        if result["result"]["status"] == "completed" {
            assert!(result.to_string().contains("Ada"));
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("task never completed");
}

/// Every request method and response status the logged server answered.
type HttpLog = Arc<std::sync::Mutex<Vec<(hyper::Method, hyper::StatusCode)>>>;

/// The real Streamable HTTP service with `test_echo`, on a loopback port,
/// logging each request's method and response status.
async fn logged_http_service() -> (std::net::SocketAddr, HttpLog) {
    use susi_http_transport::http_conn::{Builder, TokioExecutor, TokioIo};
    let service = service();
    echo(&service);
    let http = crate::server::http_service(service);
    let seen: HttpLog = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (http, log) = (http.clone(), log.clone());
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let (http, log) = (http.clone(), log.clone());
                    async move {
                        let method = req.method().clone();
                        let response = http.handle(req).await;
                        log.lock().unwrap().push((method, response.status()));
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, seen)
}

/// The blocking peer client (`susi_core::mcp_client`) against the real
/// Streamable HTTP service: version negotiation from the `initialize`
/// result, the tool call itself, and session release via `DELETE`.
#[tokio::test(flavor = "multi_thread")]
async fn peer_client_round_trip_negotiates_and_releases_session() {
    let (addr, seen) = logged_http_service().await;
    let result = tokio::task::spawn_blocking(move || {
        crate::susi_core::mcp_client::call_tool(
            &addr.to_string(),
            "test_echo",
            &json!({"ping": "pong"}),
            None,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["structuredContent"], json!({"ping": "pong"}));
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|(m, s)| m == hyper::Method::DELETE && s.is_success()),
        "session not released: {seen:?}"
    );
}

/// The std-only wire client (`susi_wire_client::mcp`, used by the thin
/// CLI) against the same real service: it negotiates the same revision,
/// gets the same `tools/call` result as the peer client, and its `DELETE`
/// releases the session.
#[tokio::test(flavor = "multi_thread")]
async fn wire_client_matches_peer_client_and_releases_session() {
    let (addr, seen) = logged_http_service().await;
    let (wire, peer, version) = tokio::task::spawn_blocking(move || {
        let addr = addr.to_string();
        let args = json!({"ping": "pong"});
        let mut session =
            susi_wire_client::mcp::Session::open(&addr, None, Duration::from_secs(5)).unwrap();
        let version = session.protocol_version().to_string();
        let wire = session
            .call_tool("test_echo", &args, None, &mut |_| {})
            .unwrap();
        session.close().unwrap();
        let peer =
            crate::susi_core::mcp_client::call_tool(&addr, "test_echo", &args, None).unwrap();
        (wire, peer, version)
    })
    .await
    .unwrap();
    assert_eq!(version, susi_wire_client::mcp::PROTOCOL_VERSION);
    assert_eq!(wire["structuredContent"], json!({"ping": "pong"}));
    assert_eq!(wire, peer);
    let seen = seen.lock().unwrap();
    let deletes = seen
        .iter()
        .filter(|(m, s)| m == hyper::Method::DELETE && s.is_success())
        .count();
    assert_eq!(deletes, 2, "both sessions must be released: {seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn http_sse_routes_subscription_ack_updates_and_request_logs() {
    use hyper::body::Bytes;
    use susi_http_transport::http_body::{BodyExt, Full};

    let svc = service();
    let log_tool: Tool = serde_json::from_value(json!({
        "name": "http_log_echo",
        "inputSchema": {"type": "object"}
    }))
    .unwrap();
    svc.register_tool(
        log_tool,
        Arc::new(|_, context| {
            Box::pin(async move {
                let threshold = context.meta.log_level();
                let _ = context
                    .emit_logging_message(
                        threshold,
                        LoggingLevel::Info,
                        Some("http-test".to_string()),
                        json!({"secret": "super-secret-value", "msg": "hi"}),
                    )
                    .await;
                Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
            })
        }),
    )
    .unwrap();
    let http = crate::server::http_service(svc.clone());

    // 1. Modern stateless POST `subscriptions/listen` streams ack + tagged updates over SSE.
    let sub_req = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "subscriptions/listen")
        .body(Full::new(Bytes::from(
            modern(
                91,
                "subscriptions/listen",
                json!({"notifications": {"resourceSubscriptions": ["http-sub://doc"]}}),
            )
            .to_string(),
        )))
        .unwrap();
    let sub_resp = http.handle(sub_req).await;
    assert_eq!(sub_resp.status(), hyper::StatusCode::OK);
    let content_type = sub_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/event-stream"),
        "expected SSE stream on subscriptions/listen: {content_type}"
    );
    let mut sub_body = sub_resp.into_body();
    let mut seen_ack = false;
    let mut seen_update = false;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !(seen_ack && seen_update) {
        let frame = tokio::time::timeout(Duration::from_secs(2), sub_body.frame())
            .await
            .expect("SSE frame timeout")
            .expect("SSE stream active")
            .expect("SSE frame ok");
        if let Some(data) = frame.data_ref() {
            buf.push_str(&String::from_utf8_lossy(data));
            for line in buf.lines() {
                if let Some(payload) = line.strip_prefix("data: ") {
                    if let Ok(val) = serde_json::from_str::<Value>(payload) {
                        if val["method"] == "notifications/subscriptions/acknowledged"
                            && val["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                                == 91
                            && !seen_ack
                        {
                            seen_ack = true;
                            svc.resource_updated("http-sub://doc".to_string());
                        }
                        if val["method"] == "notifications/resources/updated"
                            && val["params"]["uri"] == "http-sub://doc"
                            && val["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                                == 91
                        {
                            seen_update = true;
                        }
                    }
                }
            }
        }
    }
    assert!(seen_ack, "SSE stream must receive ack: {buf}");
    assert!(
        seen_update,
        "SSE stream must receive tagged resource update: {buf}"
    );
    drop(sub_body);

    // 2. Sessionful HTTP (`SessionWorker::route`) routes request-scoped logs (`target_request`)
    // onto the originating request's POST SSE stream with redaction.
    let init_req = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Full::new(Bytes::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "sse-test", "version": "1"}
                }
            })
            .to_string(),
        )))
        .unwrap();
    let init_resp = http.handle(init_req).await;
    assert_eq!(init_resp.status(), hyper::StatusCode::OK);
    let session_id = init_resp
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .expect("Mcp-Session-Id header")
        .to_string();

    let inited_req = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-11-25")
        .body(Full::new(Bytes::from(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        )))
        .unwrap();
    let inited_resp = http.handle(inited_req).await;
    assert!(inited_resp.status().is_success());

    let set_lvl = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-11-25")
        .body(Full::new(Bytes::from(
            json!({"jsonrpc": "2.0", "id": 2, "method": "logging/setLevel", "params": {"level": "info"}}).to_string(),
        )))
        .unwrap();
    let set_resp = http.handle(set_lvl).await;
    assert_eq!(set_resp.status(), hyper::StatusCode::OK);

    let session_call = hyper::Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .header("MCP-Protocol-Version", "2025-11-25")
        .body(Full::new(Bytes::from(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "http_log_echo",
                    "arguments": {},
                    "_meta": {"io.modelcontextprotocol/logLevel": "info", "progressToken": "http-sess-tok"}
                }
            })
            .to_string(),
        )))
        .unwrap();
    let session_resp = http.handle(session_call).await;
    assert_eq!(session_resp.status(), hyper::StatusCode::OK);
    let session_bytes = session_resp.into_body().collect().await.unwrap().to_bytes();
    let session_sse = String::from_utf8_lossy(&session_bytes);
    assert!(
        session_sse.contains("\"notifications/message\""),
        "sessionful POST SSE must include routed notifications/message: {session_sse}"
    );
    assert!(
        !session_sse.contains("super-secret-value") && session_sse.contains("[REDACTED]"),
        "HTTP SSE notifications/message must redact sensitive fields: {session_sse}"
    );
}

/// Wire-level test for D-SP-4 Stage 2d: sending `notifications/cancelled`
/// while `mission_run` is in a phase propagates cooperative cancellation
/// across the plane bus, stops `mission_run` at the next phase boundary, and
/// records status `"CANCELLED"` (`exit_code = 1`) in the mission trace;
/// synchronous tools other than `mission_run` keep their existing behaviour.
#[tokio::test(flavor = "multi_thread")]
async fn mission_run_cancellation_stops_at_phase_boundary_and_records_cancelled_trace() {
    use crate::susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};

    struct ToolListStub;
    impl PlaneHandler for ToolListStub {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Ok(json!([
                { "name": "mission_run", "description": "Run a mission" },
                { "name": "sovereign_status", "description": "Check status" }
            ]))
        }
    }

    struct ToolExecStub {
        mission_done: tokio::sync::Notify,
        sync_cancel_gate: Arc<std::sync::atomic::AtomicBool>,
        sync_saw_cancel: Arc<std::sync::atomic::AtomicBool>,
        sync_done: tokio::sync::Notify,
    }
    impl PlaneHandler for ToolExecStub {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            let ws = payload
                .get("workspace")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let outcome = with_watched(&payload, || match name {
                "mission_run" => {
                    crate::susi_core::progress::report("Mission started");
                    let mut status = "SUCCESS";
                    for _ in 0..400 {
                        if crate::susi_core::progress::is_cancelled() {
                            status = "CANCELLED";
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // Must not advance to the next phase when cancelled.
                    if status != "CANCELLED" {
                        crate::susi_core::progress::report("Running the agent swarm");
                    }
                    let exit_code = i32::from(status != "SUCCESS");
                    let trace_dir = ws.join(".susi");
                    let _ = std::fs::create_dir_all(&trace_dir);
                    let trace = json!({
                        "status": status,
                        "exit_code": exit_code,
                    });
                    let _ = std::fs::write(
                        trace_dir.join("last_mission_trace.json"),
                        trace.to_string(),
                    );
                    Ok(json!({ "text": trace.to_string() }))
                }
                "sovereign_status" => {
                    crate::susi_core::progress::report("Status started");
                    for _ in 0..200 {
                        if self
                            .sync_cancel_gate
                            .load(std::sync::atomic::Ordering::Acquire)
                        {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    std::thread::sleep(Duration::from_millis(60));
                    self.sync_saw_cancel.store(
                        crate::susi_core::progress::is_cancelled(),
                        std::sync::atomic::Ordering::Release,
                    );
                    Ok(json!({ "text": "ok" }))
                }
                other => Err(format!("unexpected tool {other}")),
            });
            if name == "mission_run" {
                self.mission_done.notify_waiters();
            } else if name == "sovereign_status" {
                self.sync_done.notify_waiters();
            }
            outcome
        }
    }

    let dir = susi_paths::tempfile::tempdir().unwrap();
    let exec_stub = Arc::new(ToolExecStub {
        mission_done: tokio::sync::Notify::new(),
        sync_cancel_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        sync_saw_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        sync_done: tokio::sync::Notify::new(),
    });
    // Held to the end of the test: the service reads these global topics.
    let _tools_plane = TOOLS_PLANE_TEST_LOCK.lock().await;
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(ToolListStub));
    PlaneBus::global().register(topics::TOOLS_EXECUTE, exec_stub.clone());

    let service = GmcpService::new(dir.path().to_path_buf());
    let mut wire = wire(service).await;
    discover(&mut wire).await;

    let mission_notified = exec_stub.mission_done.notified();
    let mut call = modern(
        2,
        "tools/call",
        json!({
            "name": "mission_run",
            "arguments": { "intent": "audit repo" }
        }),
    );
    call["params"]["_meta"]["progressToken"] = json!("mission-1");
    send(&mut wire, call).await;

    // Wait for "Tool started" (0.0) and "Mission started" progress notifications.
    let started = recv(&mut wire).await;
    assert_eq!(started["method"], "notifications/progress", "{started}");
    assert_eq!(started["params"]["message"], "Tool started");
    let phase = recv(&mut wire).await;
    assert_eq!(phase["method"], "notifications/progress", "{phase}");
    assert_eq!(phase["params"]["message"], "Mission started");

    // Cancel the in-flight mission_run call while it is in the phase.
    send(
        &mut wire,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 2, "reason": "user hit Ctrl-C" }
        }),
    )
    .await;

    tokio::time::timeout(Duration::from_secs(5), mission_notified)
        .await
        .expect("mission_run must stop at phase boundary after cancellation");

    let trace_text =
        std::fs::read_to_string(dir.path().join(".susi/last_mission_trace.json")).unwrap();
    let trace: Value = serde_json::from_str(&trace_text).unwrap();
    assert_eq!(trace["status"], "CANCELLED");
    assert_eq!(trace["exit_code"], 1);

    // Synchronous tools other than `mission_run` keep their existing behaviour.
    let sync_notified = exec_stub.sync_done.notified();
    let mut sync_call = modern(
        3,
        "tools/call",
        json!({
            "name": "sovereign_status",
            "arguments": {}
        }),
    );
    sync_call["params"]["_meta"]["progressToken"] = json!("status-1");
    send(&mut wire, sync_call).await;
    let _ = recv(&mut wire).await; // "Tool started"
    let sync_phase = recv(&mut wire).await;
    assert_eq!(sync_phase["params"]["message"], "Status started");
    send(
        &mut wire,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 3, "reason": "late cancel" }
        }),
    )
    .await;
    exec_stub
        .sync_cancel_gate
        .store(true, std::sync::atomic::Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), sync_notified)
        .await
        .expect("synchronous tool must run to completion");
    assert!(
        !exec_stub
            .sync_saw_cancel
            .load(std::sync::atomic::Ordering::Acquire),
        "synchronous registry tools other than mission_run must not receive a cancel flag"
    );
}

mod protocol_unit_tests {
    use super::*;
    use crate::mcp::ServerHandler;
    use crate::protocol::{compile_schema, is_tool_result_error, page, ToolHandler};

    #[test]
    fn tool_failures_remain_errors() {
        for prefix in [
            "Error:",
            "[FAIL]",
            "[CAPABILITY_GAP]",
            "Protocol Error:",
            "Reflex Error:",
            "Governance Violation:",
            "[RECOVERY]",
            "[Error]",
            "Sandbox Error:",
            "I/O Error:",
        ] {
            assert!(is_tool_result_error(&format!("  {prefix} detail")));
        }
        assert!(!is_tool_result_error("ok content"));
        assert!(!is_tool_result_error(
            "{\n  \"models\": [{\"detail\": \"Internal Engine Error: set KEY\"}]\n}"
        ));
        assert!(is_tool_result_error("[Error] {not json"));
        assert!(!is_tool_result_error(
            "Wrote 2048 bytes to notes/incident-review.txt; the file quotes an old Sandbox Error: line"
        ));
    }
    #[test]
    fn pagination_rejects_stale_and_invalid_cursors() {
        let items: Vec<_> = (0..101).collect();
        let (first, next) = page(items.clone(), None).unwrap();
        assert_eq!(first.len(), 100);
        let request = serde_json::from_value(json!({"cursor":next})).unwrap();
        assert_eq!(page(items, Some(request)).unwrap().0, vec![100]);
        let request = serde_json::from_value(json!({"cursor":next})).unwrap();
        assert!(page(vec![1, 2], Some(request)).is_err());
        let request = serde_json::from_value(json!({"cursor":"bad"})).unwrap();
        assert!(page(vec![1], Some(request)).is_err());
    }

    #[test]
    fn every_registered_mcp_tool_has_valid_typed_or_explicit_input_schema() {
        let service = GmcpService::new(std::env::temp_dir());
        let noop_handler: ToolHandler =
            Arc::new(|_req, _ctx| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
        let core_defs = crate::tools::args::core_tool_definitions();
        assert!(
            !core_defs.is_empty(),
            "CoreTools definitions must not be empty"
        );
        for def in &core_defs {
            service
                .register_tool(def.clone(), noop_handler.clone())
                .unwrap_or_else(|e| {
                    panic!(
                        "core tool `{}` failed GmcpService::register_tool: {}",
                        def.name, e.message
                    )
                });
        }

        let tools = service.tools().expect("tools catalog must load");
        assert!(
            tools.len() >= core_defs.len(),
            "MCP tools catalog must include all registered core tools"
        );

        let mut core_checked = 0usize;
        for tool in &tools {
            let schema_val = Value::Object((*tool.input_schema).clone());
            compile_schema(&schema_val).unwrap_or_else(|e| {
                panic!(
                    "tool `{}` input_schema failed compile_schema: {}",
                    tool.name, e.message
                )
            });
            assert_eq!(
                tool.input_schema.get("type").and_then(Value::as_str),
                Some("object"),
                "tool `{}` input_schema must declare root type \"object\"",
                tool.name
            );

            if crate::tools::args::core_input_schema(&tool.name).is_some() {
                core_checked += 1;
                assert!(
                    !tool
                        .input_schema
                        .contains_key(crate::tools::args::PASSTHROUGH_KEY),
                    "core tool `{}` must not use passthrough input schema",
                    tool.name
                );
                let props = tool
                    .input_schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| {
                        panic!(
                            "core tool `{}` must declare an object `properties` map",
                            tool.name
                        )
                    });
                let explicit_no_args = props.is_empty()
                    && tool
                        .input_schema
                        .get("additionalProperties")
                        .and_then(Value::as_bool)
                        == Some(false);
                assert!(
                    !props.is_empty() || explicit_no_args,
                    "core tool `{}` must have non-empty `properties` or explicit no-args (`additionalProperties: false`)",
                    tool.name
                );
            }
        }
        assert_eq!(
            core_checked,
            core_defs.len(),
            "every built-in CoreTools handler must be verified"
        );
    }

    #[test]
    fn compile_schema_enforces_depth_and_node_complexity_limits() {
        let valid_schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 }
            },
            "required": ["query"],
            "additionalProperties": false
        });
        let validator = compile_schema(&valid_schema).expect("valid object schema must compile");
        assert!(validator.is_valid(&json!({ "query": "status" })));
        assert!(!validator.is_valid(&json!({ "query": "" })));
        assert!(!validator.is_valid(&json!({})));

        // Build a schema nested beyond the 64-level depth ceiling.
        let mut deep = json!({ "type": "string" });
        for _ in 0..70 {
            deep = json!({
                "type": "object",
                "properties": { "nested": deep }
            });
        }
        let deep_err = match compile_schema(&deep) {
            Ok(_) => panic!("schema exceeding depth limit must be rejected"),
            Err(err) => err,
        };
        assert_eq!(deep_err.code, ErrorCode::INVALID_PARAMS);
        assert!(
            deep_err.message.contains("Schema exceeds complexity limit"),
            "unexpected error message: {}",
            deep_err.message
        );

        let invalid_type = json!({
            "type": "not-a-valid-json-schema-type"
        });
        let type_err = match compile_schema(&invalid_type) {
            Ok(_) => panic!("malformed schema type must be rejected"),
            Err(err) => err,
        };
        assert_eq!(type_err.code, ErrorCode::INVALID_PARAMS);
        assert!(
            type_err.message.contains("Invalid tool schema"),
            "unexpected error message: {}",
            type_err.message
        );
    }

    #[test]
    fn register_tool_replaces_catalog_entry_and_validates_arguments() {
        let service = GmcpService::new(std::env::temp_dir());
        let schema_obj: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 1 }
            },
            "required": ["count"],
            "additionalProperties": false
        }))
        .expect("schema object");
        let tool = Tool::new_with_raw(
            "custom_counter",
            Some("Count items with strict schema validation".into()),
            Arc::new(schema_obj),
        );
        let handler: ToolHandler = Arc::new(|_req, _ctx| {
            Box::pin(async {
                Ok(CallToolResult::success(vec![ContentBlock::text("counted")]).into())
            })
        });
        service
            .register_tool(tool, handler)
            .expect("custom tool with valid schema must register");

        let fetched = service
            .get_tool("custom_counter")
            .expect("registered tool must appear in get_tool");
        assert_eq!(
            fetched.description.as_deref(),
            Some("Count items with strict schema validation")
        );

        let registered = service
            .catalog
            .read()
            .tools
            .get("custom_counter")
            .cloned()
            .expect("catalog entry must exist");
        assert!(registered.input.is_valid(&json!({ "count": 3 })));
        assert!(!registered.input.is_valid(&json!({ "count": 0 })));
        assert!(!registered.input.is_valid(&json!({ "count": "three" })));
        assert!(!registered.input.is_valid(&json!({})));

        let bad_schema: JsonObject = serde_json::from_value(json!({
            "type": "invalid_type_name"
        }))
        .expect("bad schema object");
        let bad_tool = Tool::new_with_raw("bad_tool", None, Arc::new(bad_schema));
        let bad_handler: ToolHandler =
            Arc::new(|_req, _ctx| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
        assert!(
            service.register_tool(bad_tool, bad_handler).is_err(),
            "register_tool must reject invalid inputSchema"
        );
    }
}

mod task_manager_unit_tests {
    use crate::mcp::model::{ErrorCode, TaskStatus};
    use crate::mcp::task_manager::*;

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let ts = current_timestamp();
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        assert!(ts.ends_with("+00:00"), "{ts}");
    }

    #[test]
    fn uuids_are_version_4() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, uuid_v4());
    }

    #[tokio::test]
    async fn unknown_task_is_invalid_params() {
        let manager = TaskManager::new();
        for err in [
            manager.get_task("nope").unwrap_err(),
            manager.cancel_task("nope").unwrap_err(),
            manager.update_task("nope", []).unwrap_err(),
        ] {
            assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        }
    }

    #[tokio::test]
    async fn cancel_settles_to_cancelled_when_the_operation_honors_it() {
        let manager = TaskManager::new();
        let task = manager.spawn(TaskOptions::default(), |ctx| {
            Box::pin(async move {
                ctx.cancelled().await;
                Err(TaskExit::Cancelled)
            })
        });
        manager.cancel_task(&task.task_id).unwrap();
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if manager
                .get_task(&task.task_id)
                .unwrap()
                .status()
                .is_terminal()
            {
                assert_eq!(
                    manager.get_task(&task.task_id).unwrap().status(),
                    TaskStatus::Cancelled
                );
                return;
            }
        }
        panic!("task did not settle after cancel");
    }
}

mod http_unit_tests {
    use super::*;
    use crate::mcp::http::*;

    #[test]
    fn base64_header_values_decode() {
        assert_eq!(decode_header_value("plain").as_deref(), Some("plain"));
        assert_eq!(
            decode_header_value("=?base64?Y2Fmw6k=?=").as_deref(),
            Some("café")
        );
        assert_eq!(decode_header_value("=?base64?***?="), None);
    }

    #[test]
    fn event_ids_round_trip() {
        for text in ["0", "3/7"] {
            assert_eq!(text.parse::<EventId>().unwrap().to_string(), text);
        }
        assert!("x/1".parse::<EventId>().is_err());
    }

    #[test]
    fn hosts_and_origins_follow_the_allowlists() {
        let allowed = vec!["localhost".to_string(), "127.0.0.1:9090".to_string()];
        assert!(host_allowed(&("localhost".into(), Some(1)), &allowed));
        assert!(host_allowed(&("127.0.0.1".into(), Some(9090)), &allowed));
        assert!(!host_allowed(&("127.0.0.1".into(), Some(1)), &allowed));
        let origins = vec![
            "https://app.example".to_string(),
            "http://[::1]:*".to_string(),
        ];
        assert!(origin_allowed(
            &parse_origin("https://app.example:8443").unwrap(),
            &origins
        ));
        assert!(origin_allowed(
            &parse_origin("http://[::1]:3000").unwrap(),
            &origins
        ));
        assert!(!origin_allowed(
            &parse_origin("https://evil.example").unwrap(),
            &origins
        ));
    }

    #[test]
    fn sse_events_serialize_like_the_wire() {
        let priming = SseEvent::priming("0", Duration::from_secs(3));
        assert_eq!(&priming.to_bytes()[..], b"data: \nid: 0\nretry: 3000\n\n");
    }

    #[tokio::test]
    async fn bounded_lines_preserves_valid_lines_before_oversized_in_same_chunk() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let input = b"{bad\n{bad2\n123456\nok\n";
        let mut reader = BufReader::new(crate::stdio::BoundedLines::new(&input[..], 5));
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "{bad\n");
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "{bad2\n");
        line.clear();
        assert!(
            reader.read_line(&mut line).await.is_err(),
            "oversized line must error without discarding preceding lines"
        );
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "ok\n");
    }
}

mod task_extension_unit_tests {
    use super::*;
    use crate::mcp::model::{ErrorCode, InputRequest, TaskStatus};
    use crate::mcp::task_manager::*;

    fn req_with_tasks(id: i64, method: &str, params: Value) -> Value {
        let mut msg = modern(id, method, params);
        msg["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] = json!({
            "extensions": { "io.modelcontextprotocol/tasks": {} },
            "elicitation": { "form": {} }
        });
        msg
    }

    #[tokio::test]
    async fn task_manager_durable_persistence_survives_restart_and_fails_closed_on_corruption() {
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let alice = TaskPrincipal::from_bearer_token("alice-durable");

        let (done_id, inflight_id) = {
            let mgr = TaskManager::open(dir.path()).unwrap();
            let done_task = mgr
                .try_spawn_for(alice.clone(), TaskOptions::default(), |_ctx| {
                    Box::pin(
                        async move { Ok(CallToolResult::structured(json!({ "persisted": 42 }))) },
                    )
                })
                .unwrap();
            for _ in 0..100 {
                if mgr
                    .get_task_for(&done_task.task_id, &alice)
                    .unwrap()
                    .status()
                    == TaskStatus::Completed
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                mgr.get_task_for(&done_task.task_id, &alice)
                    .unwrap()
                    .status(),
                TaskStatus::Completed
            );

            let inflight_task = mgr
                .try_spawn_for(alice.clone(), TaskOptions::default(), |_ctx| {
                    Box::pin(async move {
                        std::future::pending::<()>().await;
                        Ok(CallToolResult::structured(json!({})))
                    })
                })
                .unwrap();
            assert_eq!(
                mgr.get_task_for(&inflight_task.task_id, &alice)
                    .unwrap()
                    .status(),
                TaskStatus::Working
            );
            (done_task.task_id, inflight_task.task_id)
        };

        let store_file = dir.path().join("tasks.json");
        assert!(store_file.is_file(), "durable tasks.json must be created");

        // Reopen across simulated restart.
        let reopened = TaskManager::open(dir.path()).unwrap();
        let restored_done = reopened.get_task_for(&done_id, &alice).unwrap();
        assert_eq!(restored_done.status(), TaskStatus::Completed);
        let done_json = serde_json::to_value(&restored_done).unwrap();
        assert_eq!(done_json["result"]["structuredContent"]["persisted"], 42);

        // In-flight task must settle deterministically with -32603 and diagnostic statusMessage.
        let settled_inflight = reopened.get_task_for(&inflight_id, &alice).unwrap();
        assert_eq!(settled_inflight.status(), TaskStatus::Failed);
        assert_eq!(
            settled_inflight.task.status_message.as_deref(),
            Some("task interrupted by server restart")
        );
        let err_json = serde_json::to_value(&settled_inflight).unwrap();
        assert_eq!(err_json["error"]["code"], -32603);
        drop(reopened);

        // Duplicate taskId or mismatched status/payload in tasks.json must fail closed.
        let mut raw_store: Value =
            serde_json::from_slice(&std::fs::read(&store_file).unwrap()).unwrap();
        let first_item = raw_store[1][0].clone();
        raw_store[1].as_array_mut().unwrap().push(first_item);
        std::fs::write(&store_file, serde_json::to_vec(&raw_store).unwrap()).unwrap();
        assert!(
            TaskManager::open(dir.path()).is_err(),
            "duplicate taskId in tasks.json must fail closed"
        );

        // Corrupt JSON on disk must fail closed rather than silently resetting state.
        std::fs::write(&store_file, b"{corrupt").unwrap();
        assert!(
            TaskManager::open(dir.path()).is_err(),
            "corrupt tasks.json must fail closed"
        );
    }

    #[tokio::test]
    async fn task_manager_enforces_principal_isolation_rate_limit_caps_and_immutability() {
        let alice = TaskPrincipal::from_bearer_token("alice-tok");
        let bob = TaskPrincipal::from_bearer_token("bob-tok");
        let mgr = TaskManager::new();

        // Spawn task waiting for input key "step-1".
        let task = mgr
            .try_spawn_for(
                alice.clone(),
                TaskOptions {
                    ttl_ms: Some(30_000),
                    poll_interval_ms: Some(10_000),
                    status_message: None,
                },
                |ctx| {
                    Box::pin(async move {
                        let req: InputRequest = serde_json::from_value(json!({
                            "method": "roots/list"
                        }))
                        .unwrap();
                        let ans = ctx.request_input("step-1", req).await?;
                        Ok(CallToolResult::structured(ans))
                    })
                },
            )
            .unwrap();
        for _ in 0..50 {
            if matches!(
                mgr.get_task_for(&task.task_id, &alice).unwrap().payload,
                TaskPayload::InputRequired { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        // Cross-principal must fail with -32602 across get, update, cancel, and subscription auth.
        assert_eq!(
            mgr.get_task_for(&task.task_id, &bob).unwrap_err().code,
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            mgr.update_task_for(&task.task_id, &bob, vec![("step-1".into(), json!({}))])
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            mgr.cancel_task_for(&task.task_id, &bob).unwrap_err().code,
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            mgr.authorize_tasks_for(&bob, std::slice::from_ref(&task.task_id))
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS
        );

        // Unknown/superseded keys alongside real key are ignored cleanly without panic.
        mgr.update_task_for(
            &task.task_id,
            &alice,
            vec![
                ("unknown-key".into(), json!({"ignored": true})),
                ("step-1".into(), json!({"roots": []})),
            ],
        )
        .unwrap();
        for _ in 0..100 {
            if mgr.get_task_for(&task.task_id, &alice).unwrap().status() == TaskStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            mgr.get_task_for(&task.task_id, &alice).unwrap().status(),
            TaskStatus::Completed
        );

        // Terminal immutability: update and cancel both rejected with -32602.
        assert_eq!(
            mgr.update_task_for(&task.task_id, &alice, vec![("x".into(), json!(1))])
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            mgr.cancel_task_for(&task.task_id, &alice).unwrap_err().code,
            ErrorCode::INVALID_PARAMS
        );

        // Poll-interval rate limit: exceed burst allowance on pollIntervalMs = 10_000.
        let mut rate_limited = false;
        for _ in 0..20 {
            if let Err(err) = mgr.get_task_for(&task.task_id, &alice) {
                assert_eq!(err.code, ErrorCode(-32000));
                assert!(err.message.contains("pollIntervalMs"), "{}", err.message);
                rate_limited = true;
                break;
            }
        }
        assert!(
            rate_limited,
            "rapid tasks/get must trigger -32000 rate limit"
        );

        // Active cap per principal (MAX_ACTIVE_TASKS_PER_PRINCIPAL = 64).
        let cap_mgr = TaskManager::new();
        let holder = TaskPrincipal::from_bearer_token("cap-owner");
        let mut active_ids = Vec::new();
        for _ in 0..MAX_ACTIVE_TASKS_PER_PRINCIPAL {
            let t = cap_mgr
                .try_spawn_for(holder.clone(), TaskOptions::default(), |_ctx| {
                    Box::pin(async move {
                        std::future::pending::<()>().await;
                        Ok(CallToolResult::structured(json!({})))
                    })
                })
                .unwrap();
            active_ids.push(t.task_id);
        }
        let cap_err = cap_mgr
            .try_spawn_for(holder.clone(), TaskOptions::default(), |_ctx| {
                Box::pin(async move { Ok(CallToolResult::structured(json!({}))) })
            })
            .unwrap_err();
        assert_eq!(cap_err.code, ErrorCode(-32000));

        // Input requests per task cap (> MAX_INPUT_REQUESTS_PER_TASK).
        let input_mgr = TaskManager::new();
        let req_task = input_mgr
            .try_spawn_for(holder.clone(), TaskOptions::default(), |ctx| {
                Box::pin(async move {
                    let req: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    for idx in 0..=MAX_INPUT_REQUESTS_PER_TASK {
                        let _ = ctx.request_input(format!("k-{idx}"), req.clone()).await?;
                    }
                    Ok(CallToolResult::structured(json!({})))
                })
            })
            .unwrap();
        for _ in 0..100 {
            if input_mgr
                .get_task_for(&req_task.task_id, &holder)
                .unwrap()
                .status()
                == TaskStatus::InputRequired
            {
                let key = format!("k-{}", 0);
                input_mgr
                    .update_task_for(&req_task.task_id, &holder, vec![(key, json!({}))])
                    .unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        // Oversized inputResponses key count (> MAX_INPUT_REQUESTS_PER_TASK) fails -32602.
        let too_many_updates: Vec<(String, Value)> = (0..=MAX_INPUT_REQUESTS_PER_TASK)
            .map(|i| (format!("u-{i}"), json!(1)))
            .collect();
        assert_eq!(
            input_mgr
                .update_task_for(&active_ids[0], &alice, vec![]) // unknown owner
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            cap_mgr
                .update_task_for(&active_ids[0], &holder, too_many_updates)
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS
        );
    }

    #[tokio::test]
    async fn subscriptions_listen_task_ids_gates_owner_cap_and_no_covert_leakage() {
        let svc = service();
        let gated_tool: Tool =
            serde_json::from_value(json!({"name": "sub_task", "inputSchema": {"type": "object"}}))
                .unwrap();
        svc.register_task_tool(
            gated_tool,
            Arc::new(|_, ctx| {
                Box::pin(async move {
                    let req: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    let ans = ctx.request_input("sub-k1", req).await?;
                    Ok(CallToolResult::structured(ans))
                })
            }),
        )
        .unwrap();

        let updater = svc.session();
        let mut wire = wire(svc).await;
        discover(&mut wire).await;

        // Spawn task first so we have a real owned taskId.
        send(
            &mut wire,
            req_with_tasks(2, "tools/call", json!({"name": "sub_task"})),
        )
        .await;
        let created = recv(&mut wire).await;
        assert_eq!(created["result"]["resultType"], "task", "{created}");
        let task_id = created["result"]["taskId"].as_str().unwrap().to_string();

        // 1. Missing io.modelcontextprotocol/tasks capability on subscriptions/listen.taskIds -> -32021.
        send(
            &mut wire,
            modern(
                3,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": [task_id]}}),
            ),
        )
        .await;
        let err = recv(&mut wire).await;
        assert_eq!(err["error"]["code"], -32021, "{err}");

        // 2. Over 64 taskIds -> -32602.
        let over_cap: Vec<String> = (0..=MAX_SUBSCRIPTION_TASK_IDS)
            .map(|i| format!("t-{i}"))
            .collect();
        send(
            &mut wire,
            req_with_tasks(
                4,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": over_cap}}),
            ),
        )
        .await;
        let cap_err = recv(&mut wire).await;
        assert_eq!(cap_err["error"]["code"], -32602, "{cap_err}");

        // 3. Unknown/unowned taskId rejects BEFORE emitting acknowledged.
        send(
            &mut wire,
            req_with_tasks(
                5,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": ["unknown-task-id"]}}),
            ),
        )
        .await;
        let own_err = recv(&mut wire).await;
        assert_eq!(own_err["error"]["code"], -32602, "{own_err}");

        // 4. Valid subscription emits acknowledged first, then notifications/tasks (never progress/message).
        send(
            &mut wire,
            req_with_tasks(
                6,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": [task_id]}}),
            ),
        )
        .await;
        let ack = recv(&mut wire).await;
        assert_eq!(
            ack["method"], "notifications/subscriptions/acknowledged",
            "{ack}"
        );

        let mut wire2 = super::wire(updater).await;
        discover(&mut wire2).await;
        // Complete the awaiting task on wire2 so the subscription emits notifications/tasks.
        send(
            &mut wire2,
            req_with_tasks(
                7,
                "tasks/update",
                json!({"taskId": task_id, "inputResponses": {"sub-k1": {"roots": []}}}),
            ),
        )
        .await;
        let upd_resp = recv(&mut wire2).await;
        assert!(upd_resp.get("error").is_none(), "{upd_resp}");

        let mut saw_completed_task_note = false;
        for _ in 0..8 {
            let note = recv(&mut wire).await;
            let method = note["method"].as_str().unwrap_or("");
            assert!(
                method != "notifications/progress" && method != "notifications/message",
                "task subscription stream leaked forbidden notification: {note}"
            );
            if method == "notifications/tasks" {
                assert_eq!(note["params"]["taskId"], task_id, "{note}");
                if note["params"]["status"] == "completed" {
                    assert_eq!(
                        note["params"]["result"]["structuredContent"],
                        json!({"roots": []})
                    );
                    saw_completed_task_note = true;
                    break;
                }
            }
        }
        assert!(
            saw_completed_task_note,
            "expected notifications/tasks transition to completed"
        );
    }

    #[test]
    fn jsonrpc_gate_and_transport_allowlist_task_methods_and_notifications() {
        for method in ["tasks/get", "tasks/update", "tasks/cancel"] {
            let valid = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": {
                    "taskId": "task-1",
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {
                            "extensions": { "io.modelcontextprotocol/tasks": {} }
                        }
                    }
                }
            });
            assert!(
                crate::jsonrpc_gate::http_envelope_rejection(valid.to_string().as_bytes())
                    .is_none(),
                "{method} must pass http envelope gate"
            );
        }
        assert!(crate::mcp::transport::is_standard_notification(
            "notifications/tasks"
        ));
        assert!(crate::mcp::transport::is_standard_notification(
            "notifications/subscriptions/acknowledged"
        ));
        assert!(!crate::mcp::transport::is_standard_notification(
            "notifications/bogus"
        ));
    }

    #[tokio::test]
    async fn disabling_tasks_extension_keeps_builtin_tools_sync_and_blocks_task_only_tools() {
        use susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
        struct UnitList;
        impl PlaneHandler for UnitList {
            fn handle(&self, _t: &str, _p: Value) -> Result<Value, String> {
                Ok(json!([{"name": "unit_builtin_echo", "description": "Echo"}]))
            }
        }
        struct UnitExec;
        impl PlaneHandler for UnitExec {
            fn handle(&self, _t: &str, _p: Value) -> Result<Value, String> {
                Ok(json!({"text": "sync-builtin"}))
            }
        }
        let _tools_plane = super::TOOLS_PLANE_TEST_LOCK.lock().await;
        PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(UnitList));
        PlaneBus::global().register(topics::TOOLS_EXECUTE, Arc::new(UnitExec));

        let svc = GmcpService::new(std::env::temp_dir());
        let task_tool: Tool = serde_json::from_value(json!({
            "name": "only_task_tool",
            "inputSchema": {"type": "object"}
        }))
        .unwrap();
        svc.register_task_tool(
            task_tool,
            Arc::new(|_, _| {
                Box::pin(async {
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        "async-ok",
                    )]))
                })
            }),
        )
        .unwrap();

        assert!(svc
            .extensions()
            .write()
            .set_enabled(TASKS_EXTENSION_ID, false));

        let mut wire = super::wire(svc.clone()).await;
        send(&mut wire, req_with_tasks(1, "server/discover", json!({}))).await;
        let disc = recv(&mut wire).await;
        assert!(
            disc["result"]["capabilities"]["extensions"]
                .get(TASKS_EXTENSION_ID)
                .is_none(),
            "disabled tasks extension must not be advertised: {disc}"
        );

        // Built-in tool called WITH client tasks capability runs synchronously when server disabled tasks.
        send(
            &mut wire,
            req_with_tasks(
                2,
                "tools/call",
                json!({"name": "unit_builtin_echo", "arguments": {}}),
            ),
        )
        .await;
        let sync_res = recv(&mut wire).await;
        assert_eq!(sync_res["result"]["resultType"], "complete", "{sync_res}");

        // Task-only tool fails closed with -32601 when server disabled tasks.
        send(
            &mut wire,
            req_with_tasks(
                3,
                "tools/call",
                json!({"name": "only_task_tool", "arguments": {}}),
            ),
        )
        .await;
        let blocked = recv(&mut wire).await;
        assert_eq!(blocked["error"]["code"], -32601, "{blocked}");

        // Re-enabling restores task spawning immediately on the same service instance.
        assert!(svc
            .extensions()
            .write()
            .set_enabled(TASKS_EXTENSION_ID, true));
        send(
            &mut wire,
            req_with_tasks(
                4,
                "tools/call",
                json!({"name": "only_task_tool", "arguments": {}}),
            ),
        )
        .await;
        let spawned = recv(&mut wire).await;
        assert_eq!(spawned["result"]["resultType"], "task", "{spawned}");
    }
}
