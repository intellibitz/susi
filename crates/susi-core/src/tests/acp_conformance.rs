use super::*;
use crate::mac_policy::{self, MacPolicy, PrivacyMode};
use serde_json::json;
use std::io::Cursor;
use std::sync::mpsc;

#[test]
fn discovery_and_initialize_negotiate_protocol_version() {
    let engine = AcpEngine::default();
    let doc = engine.discovery_document();
    assert_eq!(doc.schema, ACP_DISCOVERY_SCHEMA);
    assert_eq!(doc.protocol_version, ACP_PROTOCOL_VERSION);
    assert_eq!(doc.agent.license, "Apache-2.0");
    assert_eq!(doc.agent.canonical_port, ACP_CANONICAL_PORT);
    assert!(doc.capabilities.sessions);
    assert!(doc.capabilities.streaming);
    assert!(doc.capabilities.cancellation);
    assert!(doc.capabilities.capability_admission);

    for v in [ACP_PROTOCOL_VERSION, ACP_PROTOCOL_VERSION_V1] {
        let reply = engine.handle_jsonrpc_value(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocol_version": v,
                    "client_info": { "name": "test-plug", "version": "0.1.0" }
                }
            }),
            &mut |_| {},
        );
        assert_eq!(reply["result"]["protocol_version"], ACP_PROTOCOL_VERSION);
        assert_eq!(reply["result"]["requested_protocol_version"], v);
    }

    let bad = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "acp/initialize",
            "params": { "protocol_version": "0.0-draft" }
        }),
        &mut |_| {},
    );
    assert_eq!(bad["error"]["code"], JSONRPC_INVALID_PARAMS);
}

#[test]
fn mac_policy_capability_admission_refuses_ungranted_and_host_only_egress() {
    let host_only = Arc::new(MacPolicy::new([7_u8; 32], PrivacyMode::HostOnly, true));
    let engine = AcpEngine::new(
        AcpAdmissionPolicy::with_mac_policy(Arc::clone(&host_only), "corp-bridge"),
        Arc::new(SubstrateAcpExecutor),
    );

    // Baseline ACP session capabilities succeed even in HostOnly mode.
    let ok_sess = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "session/new",
            "params": {
                "workspace": "projects/demo",
                "capabilities": ["acp.session", "acp.prompt"]
            }
        }),
        &mut |_| {},
    );
    let sess_id = ok_sess["result"]["session_id"].as_str().unwrap().to_owned();
    assert!(sess_id.starts_with("acp-sess-"));

    // Requesting network.egress in HostOnly mode fails closed with -32003.
    let denied_egress = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "session/new",
            "params": {
                "capabilities": [mac_policy::actions::NETWORK_EGRESS]
            }
        }),
        &mut |_| {},
    );
    assert_eq!(denied_egress["error"]["code"], ACP_ERR_CAPABILITY_REFUSED);

    // Traversal in workspace path is also refused.
    let traversal = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "session/new",
            "params": { "workspace": "../escape" }
        }),
        &mut |_| {},
    );
    assert_eq!(traversal["error"]["code"], ACP_ERR_CAPABILITY_REFUSED);

    // Balanced policy refuses fs.write until explicitly granted to subject.
    let balanced = Arc::new(MacPolicy::new([9_u8; 32], PrivacyMode::Balanced, false));
    let engine_bal = AcpEngine::new(
        AcpAdmissionPolicy::with_mac_policy(Arc::clone(&balanced), "agent-alpha"),
        Arc::new(SubstrateAcpExecutor),
    );
    let ungranted = engine_bal.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "session/new",
            "params": {
                "workspace": "repo/sub",
                "capabilities": ["fs.write"]
            }
        }),
        &mut |_| {},
    );
    assert_eq!(ungranted["error"]["code"], ACP_ERR_CAPABILITY_REFUSED);

    balanced
        .grant("agent-alpha", "fs.write", "repo/sub", Some(300))
        .unwrap();
    let granted = engine_bal.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "session/new",
            "params": {
                "workspace": "repo/sub",
                "capabilities": ["fs.write"]
            }
        }),
        &mut |_| {},
    );
    assert_eq!(
        granted["result"]["admitted_capabilities"],
        json!(["fs.write"])
    );
}

#[test]
fn http_socket_admission_session_lifecycle_and_sse_streaming() {
    let engine = Arc::new(AcpEngine::default());
    let mut server = AcpSocketServer::bind_loopback(engine, "secret-acp-token").unwrap();
    let base_url = server.base_url();

    // Unauthenticated client can read discovery (/.well-known/acp.json), but POST /acp returns 401.
    let anon = AcpSocketClient::new(&base_url, None);
    let disc = anon.discover().unwrap();
    assert_eq!(disc.protocol_version, ACP_PROTOCOL_VERSION);
    let (unauth_status, _) = anon
        .raw_rpc(&json!({"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}))
        .unwrap();
    assert_eq!(unauth_status, 401);

    // Wrong token also returns 401.
    let wrong = AcpSocketClient::new(&base_url, Some("wrong-token".to_owned()));
    let (wrong_status, _) = wrong
        .raw_rpc(&json!({"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}))
        .unwrap();
    assert_eq!(wrong_status, 401);

    // Authenticated client runs full session/new -> session/prompt -> SSE stream -> session/close.
    let client = AcpSocketClient::new(&base_url, Some("secret-acp-token".to_owned()));
    let (st_new, new_res) = client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "session/new",
            "params": { "workspace": "src" }
        }))
        .unwrap();
    assert_eq!(st_new, 200);
    let session_id = new_res.unwrap()["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (st_prompt, prompt_res) = client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "session/prompt",
            "params": {
                "session_id": session_id,
                "prompt": [{ "type": "text", "text": "hello five pillars" }]
            }
        }))
        .unwrap();
    assert_eq!(st_prompt, 200);
    let prompt_json = prompt_res.unwrap();
    assert_eq!(prompt_json["result"]["stop_reason"], "end_turn");
    assert_eq!(
        prompt_json["result"]["receipt_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );

    let (st_sse, sse_frames) = client
        .prompt_stream_sse(22, &session_id, "streamed turn")
        .unwrap();
    assert_eq!(st_sse, 200);
    let event_names: Vec<&str> = sse_frames.iter().map(|(n, _)| n.as_str()).collect();
    assert!(event_names.contains(&"acp.turn.event"), "{event_names:?}");
    assert_eq!(event_names.last().copied(), Some("acp.turn.result"));

    let (_, loaded) = client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 23,
            "method": "session/load",
            "params": { "session_id": session_id }
        }))
        .unwrap();
    assert_eq!(loaded.unwrap()["result"]["session"]["turns_completed"], 2);

    let (_, closed) = client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 24,
            "method": "session/close",
            "params": { "session_id": session_id }
        }))
        .unwrap();
    assert_eq!(closed.unwrap()["result"]["closed"], true);

    let (_, after_close) = client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 25,
            "method": "session/load",
            "params": { "session_id": session_id }
        }))
        .unwrap();
    assert_eq!(after_close.unwrap()["error"]["code"], ACP_ERR_NOT_FOUND);

    server.shutdown();
}

struct BlockingExecutor {
    entered_tx: Mutex<Option<mpsc::Sender<()>>>,
}

impl AcpTurnExecutor for BlockingExecutor {
    fn execute_turn(
        &self,
        _ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        emit_delta("step-1");
        if let Some(tx) = self
            .entered_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(());
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cancelled.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(vec![AcpContentBlock::text(
            "should-not-complete-if-cancelled",
        )])
    }
}

#[test]
fn cooperative_mid_turn_cancellation_transitions_to_cancelled_stop_reason() {
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::new(BlockingExecutor {
            entered_tx: Mutex::new(Some(tx)),
        }),
    ));
    let created = engine.handle_jsonrpc_value(
        json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}),
        &mut |_| {},
    );
    let session_id = created["result"]["session_id"].as_str().unwrap().to_owned();

    let engine_turn = Arc::clone(&engine);
    let sid_turn = session_id.clone();
    let worker = std::thread::spawn(move || {
        engine_turn.handle_jsonrpc_value(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/prompt",
                "params": {
                    "session_id": sid_turn,
                    "turn_id": "turn-live-1",
                    "prompt": "long task"
                }
            }),
            &mut |_| {},
        )
    });

    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let cancel_reply = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/cancel",
            "params": { "session_id": session_id, "turn_id": "turn-live-1" }
        }),
        &mut |_| {},
    );
    assert_eq!(cancel_reply["result"]["cancelled"], true);

    let turn_reply = worker.join().unwrap();
    assert_eq!(turn_reply["result"]["stop_reason"], "cancelled");
    assert!(turn_reply["result"]["output"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn stdio_ndjson_transport_and_jsonrpc_error_codes_conform() {
    let engine = AcpEngine::default();
    let input = concat!(
        "{not-json}\n",
        "{\"jsonrpc\":\"1.0\",\"id\":1,\"method\":\"initialize\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"nope/method\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"acp/discover\",\"params\":{}}\n"
    );
    let mut out = Vec::new();
    let count =
        AcpStdioTransport::serve_reader_writer(&engine, Cursor::new(input.as_bytes()), &mut out)
            .unwrap();
    assert_eq!(count, 4);
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["error"]["code"], JSONRPC_PARSE_ERROR);
    assert_eq!(lines[1]["error"]["code"], JSONRPC_INVALID_REQUEST);
    assert_eq!(lines[2]["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
    assert_eq!(lines[3]["result"]["schema"], ACP_DISCOVERY_SCHEMA);
}
