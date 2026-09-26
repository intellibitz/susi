//! Wire-level tests exercise the same rmcp service used by both entry points.
use crate::protocol::GmcpService;
use rmcp::{model::*, ServiceExt};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

type Wire = BufReader<DuplexStream>;
fn service() -> GmcpService {
    GmcpService::new(std::path::PathBuf::from("."))
}
async fn wire(service: GmcpService) -> Wire {
    let (client, server) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let running = service.serve(server).await.unwrap();
        running.waiting().await.unwrap();
    });
    BufReader::new(client)
}
async fn send(wire: &mut Wire, message: Value) {
    wire.get_mut()
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}
async fn recv(wire: &mut Wire) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), wire.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
fn modern(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":{}});
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}
async fn discover(wire: &mut Wire) {
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
    send(&mut wire, modern(2, "tools/list", json!({}))).await;
    let listed = recv(&mut wire).await;
    assert!(listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "test_echo" && t["inputSchema"]["type"] == "object"));
    assert!(listed["result"].get("ttlMs").is_some(), "{listed}");
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
            Err(rmcp::task_manager::TaskExit::Cancelled)
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
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use tower_service::Service;
    let mut http = crate::server::http_service(service());
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
        .call(request("server/discover", "localhost", None))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::OK);
    assert!(response.headers().get("Mcp-Session-Id").is_none());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("susi-gmcp"));
    let response = http
        .call(request("tools/list", "localhost", None))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::BAD_REQUEST);
    let response = http
        .call(request("server/discover", "attacker.invalid", None))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
    let response = http
        .call(request(
            "server/discover",
            "localhost",
            Some("https://attacker.invalid"),
        ))
        .await
        .unwrap();
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
