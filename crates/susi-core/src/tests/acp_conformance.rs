use super::*;
use crate::mac_policy::{self, MacPolicy, PrivacyMode};
use serde_json::json;
use std::io::{Cursor, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

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

#[test]
fn acp_client_callback_observes_sse_before_the_final_frame() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let (server_started_tx, server_started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        let first_frame = b"event: acp.turn.event\ndata: {\"phase\":\"started\"}\n\n";
        let final_frame = b"event: acp.turn.result\ndata: {\"stop_reason\":\"end_turn\"}\n\n";
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            first_frame.len() + final_frame.len()
        );
        stream
            .write_all(head.as_bytes())
            .and_then(|_| stream.write_all(first_frame))
            .unwrap();
        stream.flush().unwrap();
        server_started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        stream.write_all(final_frame).unwrap();
        stream.flush().unwrap();
    });

    let (observed_tx, observed_rx) = mpsc::channel();
    let client = AcpSocketClient::new(format!("http://{addr}"), None);
    let client_thread = std::thread::spawn(move || {
        let mut frames = Vec::new();
        let result = client.prompt_stream_sse_with(1, "acp-sess-live", "hello", |frame| {
            observed_tx.send(frame.clone()).unwrap();
            frames.push(frame);
            Ok(())
        });
        (result, frames)
    });

    server_started_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let first = observed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first SSE frame must arrive before the server releases the final frame");
    assert_eq!(first.0, "acp.turn.event");
    assert_eq!(first.1["phase"], "started");
    release_tx.send(()).unwrap();

    let (result, frames) = client_thread.join().unwrap();
    assert_eq!(result.unwrap(), 200);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].0, "acp.turn.result");
    server.join().unwrap();
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

struct MultiTurnToolExecutor {
    seen_contexts: Mutex<Vec<Vec<AcpInferenceMessage>>>,
}

impl AcpTurnExecutor for MultiTurnToolExecutor {
    fn execute_turn(
        &self,
        _ctx: &AcpTurnContext,
        _cancelled: &AtomicBool,
        _emit_delta: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        Ok(Vec::new())
    }

    fn execute_turn_with_tools(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> Result<AcpExecutorOutput, String> {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(AcpExecutorOutput::default());
        }
        let inf = ctx.inference_messages();
        self.seen_contexts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(inf.clone());
        let reply = format!("turn={} ctx_len={}", ctx.turn_id, inf.len());
        emit_delta(&reply);
        let tool_calls = if ctx.role == AcpMessageRole::User && ctx.turn_id == "t-1" {
            vec![AcpToolCallMeta {
                id: "call-read-1".to_owned(),
                name: "fs.read".to_owned(),
                arguments: json!({ "path": "README.md" }),
                status: Some("requested".to_owned()),
            }]
        } else {
            Vec::new()
        };
        Ok(AcpExecutorOutput {
            blocks: vec![AcpContentBlock::text(reply)],
            tool_calls,
        })
    }
}

#[test]
fn multi_turn_session_accumulates_messages_tool_calls_and_inference_context() {
    let exec = Arc::new(MultiTurnToolExecutor {
        seen_contexts: Mutex::new(Vec::new()),
    });
    let engine = AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::clone(&exec) as Arc<dyn AcpTurnExecutor>,
    );
    assert!(engine.discovery_document().capabilities.session_history);

    // Seed session/new with system_prompt and an initial user/assistant exchange.
    let created = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {
                "workspace": "crates/susi-core",
                "system_prompt": "You are Susi Core ACP assistant.",
                "messages": [
                    { "role": "user", "content": "prior question", "turn_id": "seed-u1" },
                    { "role": "assistant", "content": "prior answer", "turn_id": "seed-u1" }
                ]
            }
        }),
        &mut |_| {},
    );
    let session_id = created["result"]["session_id"].as_str().unwrap().to_owned();
    assert_eq!(created["result"]["message_count"], 3);

    // Turn 1: user prompt emits assistant tool_call metadata.
    let mut streamed_kinds = Vec::new();
    let t1 = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "session_id": session_id,
                "turn_id": "t-1",
                "prompt": "inspect README"
            }
        }),
        &mut |ev| streamed_kinds.push(ev.kind),
    );
    assert!(streamed_kinds.iter().any(|k| k == "tool_call"));
    assert_eq!(t1["result"]["tool_calls"][0]["name"], "fs.read");
    assert_eq!(t1["result"]["messages"].as_array().unwrap().len(), 5);
    assert_eq!(
        t1["result"]["inference_messages"].as_array().unwrap().len(),
        5
    );

    // Turn 2: tool result follow-up turn carries role="tool" and tool_calls metadata.
    let t2 = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "turnId": "t-2",
                "role": "tool",
                "content": "# Susi README contents",
                "tool_calls": [{
                    "id": "call-read-1",
                    "name": "fs.read",
                    "arguments": { "bytes": 22 },
                    "status": "succeeded"
                }]
            }
        }),
        &mut |_| {},
    );
    assert_eq!(t2["result"]["stop_reason"], "end_turn");
    assert_eq!(t2["result"]["messages"].as_array().unwrap().len(), 7);

    // Verify executor saw growing multi-turn context: 4 messages on t-1 (3 seeds + t-1 prompt),
    // and 6 messages on t-2 (3 seeds + t-1 user/assistant + t-2 tool result).
    let seen = exec.seen_contexts.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].len(), 4);
    assert_eq!(seen[0][0].role, AcpMessageRole::System);
    assert_eq!(seen[1].len(), 6);
    assert_eq!(seen[1][4].tool_calls[0].id, "call-read-1");
    assert_eq!(seen[1][5].role, AcpMessageRole::Tool);
}

#[test]
fn bounded_ring_enforces_message_cap_byte_budget_and_cursor_replay() {
    let policy = AcpAdmissionPolicy::default().with_history_bounds(4, 180);
    let engine = AcpEngine::new(policy, Arc::new(SubstrateAcpExecutor));

    let created = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": { "system_prompt": "sys" }
        }),
        &mut |_| {},
    );
    let sid = created["result"]["session_id"].as_str().unwrap().to_owned();

    // Run 4 prompt turns (8 messages + 1 system = 9 total pushed -> ring cap 4 & byte cap 180).
    for idx in 1..=4 {
        let res = engine.handle_jsonrpc_value(
            json!({
                "jsonrpc": "2.0",
                "id": idx + 1,
                "method": "session/prompt",
                "params": {
                    "session_id": sid,
                    "turn_id": format!("turn-{idx}"),
                    "prompt": format!("bounded payload segment number {idx} with UTF-8 café")
                }
            }),
            &mut |_| {},
        );
        let bounds: AcpHistoryBounds =
            serde_json::from_value(res["result"]["history_bounds"].clone()).unwrap();
        assert!(bounds.retained_messages <= 4);
        assert!(bounds.retained_bytes <= 180);
    }

    // Even a single oversized UTF-8 message clamps cleanly within max_bytes without panicking.
    let huge = "🚀".repeat(120); // 480 bytes > 180 byte budget
    let clamp_res = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "session/prompt",
            "params": { "session_id": sid, "turn_id": "turn-huge", "prompt": huge }
        }),
        &mut |_| {},
    );
    let final_bounds: AcpHistoryBounds =
        serde_json::from_value(clamp_res["result"]["history_bounds"].clone()).unwrap();
    assert!(final_bounds.retained_messages <= 4);
    assert!(final_bounds.retained_bytes <= 180);
    assert!(final_bounds.evicted_messages >= 5);

    // Cursor replay via session/history with limit=1 paginates deterministically.
    let page1 = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "session/history",
            "params": { "session_id": sid, "since_seq": 0, "limit": 1 }
        }),
        &mut |_| {},
    );
    let page1_msgs = page1["result"]["messages"].as_array().unwrap();
    assert_eq!(page1_msgs.len(), 1);
    let cursor = page1["result"]["next_seq"].as_u64().unwrap();
    assert!(cursor >= final_bounds.oldest_seq.unwrap());

    if page1["result"]["has_more"].as_bool().unwrap() {
        let page2 = engine.handle_jsonrpc_value(
            json!({
                "jsonrpc": "2.0",
                "id": 22,
                "method": "session/history",
                "params": { "session_id": sid, "since_seq": cursor, "limit": 10 }
            }),
            &mut |_| {},
        );
        let page2_msgs = page2["result"]["messages"].as_array().unwrap();
        assert!(!page2_msgs.is_empty());
        assert!(page2_msgs[0]["seq"].as_u64().unwrap() > cursor);
        assert_eq!(page2["result"]["has_more"], false);
    }
}

#[test]
fn cancelled_turns_record_audit_history_while_staying_out_of_inference_messages_and_header_guard_enforces_session_id(
) {
    let engine = Arc::new(AcpEngine::default());
    let mut server = AcpSocketServer::bind_loopback(Arc::clone(&engine), "tok-multi").unwrap();
    let client = AcpSocketClient::new(server.base_url(), Some("tok-multi".to_owned()));

    let (_, created) = client
        .raw_rpc(&json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}))
        .unwrap();
    let sid = created.unwrap()["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Pre-cancel next turn on session, then run cancelled prompt followed by a normal prompt.
    let (_, _) = client
        .raw_rpc_with_headers(
            &json!({"jsonrpc":"2.0","id":2,"method":"session/cancel","params":{}}),
            &[(ACP_SESSION_ID_HEADER, &sid)],
        )
        .unwrap();
    let (_, cancelled_turn) = client
        .raw_rpc_with_headers(
            &json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"prompt":"aborted"}}),
            &[(ACP_SESSION_ID_HEADER, &sid)],
        )
        .unwrap();
    let ct_val = cancelled_turn.unwrap();
    assert_eq!(ct_val["result"]["stop_reason"], "cancelled");

    let (_, ok_turn) = client
        .raw_rpc_with_headers(
            &json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"prompt":"kept"}}),
            &[(ACP_SESSION_ID_HEADER, &sid)],
        )
        .unwrap();
    let ok_val = ok_turn.unwrap();
    assert_eq!(ok_val["result"]["stop_reason"], "end_turn");

    // session_load shows turns_completed=1, turns_cancelled=1, 4 audit messages, 2 inference_messages!
    let loaded = client.session_load(5, &sid).unwrap();
    assert_eq!(loaded["result"]["session"]["turns_completed"], 1);
    assert_eq!(loaded["result"]["session"]["turns_cancelled"], 1);
    assert_eq!(loaded["result"]["messages"].as_array().unwrap().len(), 4);
    assert_eq!(
        loaded["result"]["inference_messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // session/history with include_cancelled=false filters out cancelled entries cleanly.
    let (_, non_cancelled_hist) = client
        .raw_rpc_with_headers(
            &json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "session/history",
                "params": { "include_cancelled": false }
            }),
            &[(ACP_SESSION_ID_HEADER, &sid)],
        )
        .unwrap();
    assert_eq!(
        non_cancelled_hist.unwrap()["result"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Conflicting Acp-Session-Id HTTP header vs JSON-RPC params.session_id is rejected with -32602.
    let (st_conflict, conflict_body) = client
        .raw_rpc_with_headers(
            &json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "session/load",
                "params": { "session_id": "acp-sess-spoofed" }
            }),
            &[(ACP_SESSION_ID_HEADER, &sid)],
        )
        .unwrap();
    assert_eq!(st_conflict, 200);
    assert_eq!(
        conflict_body.unwrap()["error"]["code"],
        JSONRPC_INVALID_PARAMS
    );

    server.shutdown();
}

#[test]
fn session_eviction_cleans_up_turn_cancellations_for_evicted_session() {
    let policy = AcpAdmissionPolicy {
        max_sessions: 1,
        ..AcpAdmissionPolicy::default()
    };
    let engine = AcpEngine::new(policy, Arc::new(SubstrateAcpExecutor));

    let s1 = engine.handle_jsonrpc_value(
        json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}),
        &mut |_| {},
    );
    let sid1 = s1["result"]["session_id"].as_str().unwrap().to_owned();

    // Register a turn-specific cancellation entry for sid1 before starting that turn.
    let cancelled = engine.handle_jsonrpc_value(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/cancel",
            "params": { "session_id": sid1, "turn_id": "turn-pending-evict" }
        }),
        &mut |_| {},
    );
    assert_eq!(cancelled["result"]["cancelled"], true);
    {
        let guard = engine.state.lock().unwrap();
        assert!(guard
            .turn_cancellations
            .contains_key(&(sid1.clone(), "turn-pending-evict".to_owned())));
    }

    // Creating a second session evicts sid1 and must purge its turn_cancellations entries.
    let s2 = engine.handle_jsonrpc_value(
        json!({"jsonrpc":"2.0","id":3,"method":"session/new","params":{}}),
        &mut |_| {},
    );
    let sid2 = s2["result"]["session_id"].as_str().unwrap().to_owned();
    assert_ne!(sid1, sid2);

    let guard = engine.state.lock().unwrap();
    assert!(!guard.sessions.contains_key(&sid1));
    assert!(guard.turn_cancellations.keys().all(|(sid, _)| sid != &sid1));
}
