/// GMCP-203-014-04: deterministic adversarial, negative, attack, concurrency-race,
/// crash/corruption, and fault-injection suite covering threats `T1..T8` and
/// fixtures `F-NEG-014-01..08` of `docs/gmcp/contract-203-014.md`.
#[test]
fn gmcp_req_203_014_04_adversarial() {
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};
    use susi_gmcp::jsonrpc_gate::{
        classify_envelope, classify_line, http_envelope_rejection, EraState, GateVerdict,
    };
    use susi_gmcp::mcp::model::{
        CallToolResult, ContentBlock, ErrorCode, ErrorData, InputRequest, TaskStatus as SrvStatus,
        Tool, TASKS_EXTENSION_ID,
    };
    use susi_gmcp::mcp::service::MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION;
    use susi_gmcp::mcp::task_manager::{
        uuid_v4, TaskExit, TaskManager, TaskOptions, TaskPrincipal, MAX_ACTIVE_TASKS_PER_PRINCIPAL,
        MAX_GLOBAL_ACTIVE_TASKS, MAX_INPUT_REQUESTS_PER_TASK, MAX_INPUT_RESPONSE_BYTES,
        MAX_RETAINED_TERMINAL_TASKS, MAX_SUBSCRIPTION_TASK_IDS, MAX_TASK_TTL_MS,
    };
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::server::GmcpServer;
    use susi_wire_client::mcp::{
        CallToolOutcome, PersistedTaskHandle, PersistedTaskHandleStore, Session, TaskHandle,
        TaskSnapshot, TaskStatus, WireError, PROTOCOL_VERSION_2026_07_28,
    };

    let (_http_lock, _http_env, home) = gmcp_http_home_guard("014-adv");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    susi_daemon::composition::wire_mcp_serve_substrate(&home.join(".susi"));

    let tasks_meta = |req_id: &str| -> Value {
        json!({ "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28, "io.modelcontextprotocol/requestId": req_id, "io.modelcontextprotocol/clientCapabilities": { "extensions": { "io.modelcontextprotocol/tasks": {}}}})
    };
    let plain_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };

    // ── 1. T1 & T2: CSPRNG taskId entropy, secret hashing privacy & anti-oracle ──
    let mut ids = BTreeSet::new();
    let mut leading_bytes = BTreeSet::new();
    for _ in 0..256 {
        let id = uuid_v4();
        assert_eq!(id.len(), 36, "UUIDv4 length invariant: {id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|s| s.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "RFC 4122 hyphenation invariant: {id}"
        );
        assert!(
            parts[2].starts_with('4'),
            "UUIDv4 version nibble must be 4: {id}"
        );
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "RFC 4122 variant nibble must be 8/9/a/b: {id}"
        );
        leading_bytes.insert(parts[0][..2].to_string());
        assert!(ids.insert(id), "CSPRNG uuid_v4 collision detected");
    }
    assert!(
        leading_bytes.len() >= 64,
        "CSPRNG entropy spread too narrow across 256 samples: {}",
        leading_bytes.len()
    );

    let raw_secret = "super-secret-bearer-token-value-do-not-leak";
    let alice_p = TaskPrincipal::from_bearer_token(raw_secret);
    let bob_p = TaskPrincipal::from_bearer_token("distinct-bob-bearer-secret");
    let node_a = TaskPrincipal::from_node("node-alpha");
    let node_b = TaskPrincipal::from_node("node-beta");
    assert!(!alice_p.as_str().contains(raw_secret));
    assert!(alice_p.as_str().starts_with("bearer:"));
    assert_eq!(alice_p.as_str().len(), "bearer:".len() + 32);
    assert_ne!(alice_p, bob_p);
    assert_ne!(node_a, node_b);
    assert_ne!(alice_p, TaskPrincipal::local());

    // ── 2. T3, T4, T5, T6, T7: Durable store corruption, fault-injection rollback,
    //    TTL clamping/expiry/purge, caps, rate limits, MRTR abuse & concurrency races ──
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        // 2a. Durable privacy, crash reconciliation & corruption/tamper refusal (`T2`, `T5`, `F-NEG-014-08`).
        let crash_dir = home.join("crash-store");
        let side_effects = Arc::new(AtomicUsize::new(0));
        let (done_id, cancel_id, work_id, input_id) = {
            let mgr = TaskManager::open(&crash_dir).unwrap();
            let fx = side_effects.clone();
            let done_t = mgr
                .try_spawn_for(
                    alice_p.clone(),
                    TaskOptions {
                        ttl_ms: Some(u64::MAX), // Clamped to MAX_TASK_TTL_MS (`T3`)
                        poll_interval_ms: Some(5),
                        status_message: Some("initial".into()),
                    },
                    move |_ctx| {
                        Box::pin(async move {
                            fx.fetch_add(1, Ordering::SeqCst);
                            Ok(CallToolResult::structured(json!({"once": true})))
                        })
                    },
                )
                .unwrap();
            assert_eq!(
                done_t.ttl_ms,
                Some(MAX_TASK_TTL_MS),
                "excessive ttl_ms must clamp to MAX_TASK_TTL_MS"
            );
            // Immediate durability (`-0300`): resolvable right after spawn returns.
            assert!(mgr.get_task_for(&done_t.task_id, &alice_p).is_ok());

            for _ in 0..100 {
                if mgr.get_task_for(&done_t.task_id, &alice_p).unwrap().status()
                    == SrvStatus::Completed
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                mgr.get_task_for(&done_t.task_id, &alice_p).unwrap().status(),
                SrvStatus::Completed
            );

            let cancel_t = mgr
                .try_spawn_for(alice_p.clone(), TaskOptions::default(), |ctx| {
                    Box::pin(async move {
                        ctx.cancelled().await;
                        Err(TaskExit::Cancelled)
                    })
                })
                .unwrap();
            mgr.cancel_task_for(&cancel_t.task_id, &alice_p).unwrap();
            for _ in 0..100 {
                if mgr
                    .get_task_for(&cancel_t.task_id, &alice_p)
                    .unwrap()
                    .status()
                    == SrvStatus::Cancelled
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let work_t = mgr
                .try_spawn_for(alice_p.clone(), TaskOptions::default(), |_ctx| {
                    Box::pin(async move {
                        std::future::pending::<()>().await;
                        Ok(CallToolResult::structured(json!({})))
                    })
                })
                .unwrap();

            let input_t = mgr
                .try_spawn_for(alice_p.clone(), TaskOptions::default(), |ctx| {
                    Box::pin(async move {
                        let req: InputRequest =
                            serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                        let _ = ctx.request_input("crash-k1", req).await?;
                        Ok(CallToolResult::structured(json!({})))
                    })
                })
                .unwrap();
            for _ in 0..100 {
                if mgr.get_task_for(&input_t.task_id, &alice_p).unwrap().status()
                    == SrvStatus::InputRequired
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            (
                done_t.task_id,
                cancel_t.task_id,
                work_t.task_id,
                input_t.task_id,
            )
        };

        let store_path = crash_dir.join("tasks.json");
        let on_disk_text = std::fs::read_to_string(&store_path).unwrap();
        assert!(
            !on_disk_text.contains(raw_secret),
            "raw bearer token must never be persisted to tasks.json"
        );

        let reopened = TaskManager::open(&crash_dir).unwrap();
        assert_eq!(side_effects.load(Ordering::SeqCst), 1, "completed task must not re-run");
        assert_eq!(
            reopened.get_task_for(&done_id, &alice_p).unwrap().status(),
            SrvStatus::Completed
        );
        assert_eq!(
            reopened.get_task_for(&cancel_id, &alice_p).unwrap().status(),
            SrvStatus::Cancelled
        );
        for orphaned_id in [&work_id, &input_id] {
            let reconciled = reopened.get_task_for(orphaned_id, &alice_p).unwrap();
            assert_eq!(reconciled.status(), SrvStatus::Failed);
            assert_eq!(
                reconciled.task.status_message.as_deref(),
                Some("task interrupted by server restart")
            );
            let wire_val = serde_json::to_value(&reconciled).unwrap();
            assert_eq!(wire_val["error"]["code"], -32603);
            assert!(wire_val.get("result").is_none());
            assert!(wire_val.get("inputRequests").is_none());
        }
        let valid_snapshot_bytes = std::fs::read(&store_path).unwrap();
        drop(reopened);

        // Corrupt / tampered durable store matrix must fail closed on startup (`F-NEG-014-08`).
        for (label, bad_bytes) in [
            ("truncated-json", b"{not-valid-json".to_vec()),
            ("wrong-version", serde_json::to_vec(&json!([99, []])).unwrap()),
            ("empty-task-id", {
                let mut v: Value = serde_json::from_slice(&valid_snapshot_bytes).unwrap();
                v[1][0][1]["taskId"] = json!("");
                serde_json::to_vec(&v).unwrap()
            }),
            ("duplicate-task-id", {
                let mut v: Value = serde_json::from_slice(&valid_snapshot_bytes).unwrap();
                let dup = v[1][0].clone();
                v[1].as_array_mut().unwrap().push(dup);
                serde_json::to_vec(&v).unwrap()
            }),
            ("status-payload-mismatch", {
                let mut v: Value = serde_json::from_slice(&valid_snapshot_bytes).unwrap();
                v[1][0][1]["status"] = json!("working"); // payload is Completed
                serde_json::to_vec(&v).unwrap()
            }),
        ] {
            std::fs::write(&store_path, &bad_bytes).unwrap();
            assert!(
                TaskManager::open(&crash_dir).is_err(),
                "TaskManager::open must fail closed on {label}"
            );
            assert!(
                GmcpService::open_durable(root.to_path_buf(), crash_dir.clone()).is_err(),
                "GmcpService::open_durable must fail closed on {label}"
            );
        }

        // 2b. Persistence write fault-injection & atomic rollback (`T5`).
        let fault_dir = home.join("fault-rollback");
        let fault_mgr = TaskManager::open(&fault_dir).unwrap();
        let fault_target = fault_dir.join("tasks.json");
        std::fs::remove_file(&fault_target).unwrap();
        std::fs::create_dir_all(fault_target.join("non-empty-blocker")).unwrap();
        let fault_runs = Arc::new(AtomicUsize::new(0));
        let fr = fault_runs.clone();
        let spawn_err = fault_mgr
            .try_spawn_for(alice_p.clone(), TaskOptions::default(), move |_ctx| {
                Box::pin(async move {
                    fr.fetch_add(1, Ordering::SeqCst);
                    Ok(CallToolResult::structured(json!({"should_not": "run"})))
                })
            })
            .expect_err("unwritable/blocked tasks.json target must fail try_spawn_for closed");
        assert_eq!(spawn_err.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            fault_mgr.running_task_count(),
            0,
            "failed persist must roll back in-memory TaskEntry"
        );
        assert_eq!(
            fault_runs.load(Ordering::SeqCst),
            0,
            "failed persist must never execute task future"
        );
        let leftover_tmps = std::fs::read_dir(&fault_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("tasks.json.tmp.")
            })
            .count();
        assert_eq!(leftover_tmps, 0, "failed persist must clean up temporary file");

        // Recover cleanly once filesystem fault is cleared.
        std::fs::remove_dir_all(&fault_target).unwrap();
        let recovered = fault_mgr
            .try_spawn_for(alice_p.clone(), TaskOptions::default(), |_ctx| {
                Box::pin(async move { Ok(CallToolResult::structured(json!({"recovered": true}))) })
            })
            .expect("TaskManager must recover cleanly after transient disk fault");
        assert!(fault_mgr.get_task_for(&recovered.task_id, &alice_p).is_ok());

        // 2c. TTL expiry & retention purge (`T3`, `F-NEG-014-03`).
        let ttl_mgr = TaskManager::new();
        let exp_task = ttl_mgr
            .try_spawn_for(
                alice_p.clone(),
                TaskOptions {
                    ttl_ms: Some(25),
                    poll_interval_ms: Some(5),
                    status_message: None,
                },
                |_ctx| {
                    Box::pin(async move {
                        std::future::pending::<()>().await;
                        Ok(CallToolResult::structured(json!({})))
                    })
                },
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(35)).await;
        // First sweep marks active expired task as Failed (`ttlMs` retention starts at settlement);
        // waiting another >25ms purges the expired terminal record completely (`-32602`).
        let failed_exp = ttl_mgr.get_task_for(&exp_task.task_id, &alice_p).unwrap();
        assert_eq!(failed_exp.status(), SrvStatus::Failed);
        assert_eq!(
            failed_exp.task.status_message.as_deref(),
            Some("task expired: TTL elapsed")
        );
        tokio::time::sleep(Duration::from_millis(35)).await;
        for purged_err in [
            ttl_mgr.get_task_for(&exp_task.task_id, &alice_p).unwrap_err(),
            ttl_mgr
                .update_task_for(&exp_task.task_id, &alice_p, Vec::new())
                .unwrap_err(),
            ttl_mgr
                .cancel_task_for(&exp_task.task_id, &alice_p)
                .unwrap_err(),
        ] {
            assert_eq!(purged_err.code, ErrorCode::INVALID_PARAMS);
        }

        // Uniform `-32602` anti-oracle across nonexistent, traversal, NUL, and unauthorized handles (`T1`, `T2`).
        let live_alice = ttl_mgr
            .try_spawn_for(alice_p.clone(), TaskOptions::default(), |ctx| {
                Box::pin(async move {
                    ctx.cancelled().await;
                    Err(TaskExit::Cancelled)
                })
            })
            .unwrap();
        for (caller, probe_id) in [
            (&bob_p, live_alice.task_id.as_str()),
            (&node_a, live_alice.task_id.as_str()),
            (&TaskPrincipal::local(), live_alice.task_id.as_str()),
            (&alice_p, "00000000-0000-4000-8000-ffffffffffff"),
            (&alice_p, "../../etc/passwd"),
            (&alice_p, "bad\u{0000}nul"),
            (&alice_p, "' OR '1'='1"),
        ] {
            assert_eq!(
                ttl_mgr.get_task_for(probe_id, caller).unwrap_err().code,
                ErrorCode::INVALID_PARAMS
            );
            assert_eq!(
                ttl_mgr
                    .update_task_for(probe_id, caller, Vec::new())
                    .unwrap_err()
                    .code,
                ErrorCode::INVALID_PARAMS
            );
            assert_eq!(
                ttl_mgr.cancel_task_for(probe_id, caller).unwrap_err().code,
                ErrorCode::INVALID_PARAMS
            );
            assert_eq!(
                ttl_mgr
                    .authorize_tasks_for(caller, &[probe_id.to_string()])
                    .unwrap_err()
                    .code,
                ErrorCode::INVALID_PARAMS
            );
        }
        ttl_mgr
            .cancel_task_for(&live_alice.task_id, &alice_p)
            .unwrap();

        // 2d. Per-principal (64), global (512), and terminal retention (2048) resource bounds (`T7`).
        let quota_mgr = TaskManager::new();
        let mut p0_ids = Vec::new();
        for p_idx in 0..8 {
            let owner = TaskPrincipal::from_node(&format!("quota-node-{p_idx}"));
            for _ in 0..MAX_ACTIVE_TASKS_PER_PRINCIPAL {
                let t = quota_mgr
                    .try_spawn_for(owner.clone(), TaskOptions::default(), |ctx| {
                        Box::pin(async move {
                            ctx.cancelled().await;
                            Err(TaskExit::Cancelled)
                        })
                    })
                    .unwrap();
                if p_idx == 0 {
                    p0_ids.push(t.task_id);
                }
            }
            // 65th task for the same principal fails with -32000.
            let p_cap_err = quota_mgr
                .try_spawn_for(owner.clone(), TaskOptions::default(), |_ctx| {
                    Box::pin(async move { Ok(CallToolResult::structured(json!({}))) })
                })
                .unwrap_err();
            assert_eq!(p_cap_err.code, ErrorCode(-32000));
        }
        assert_eq!(quota_mgr.running_task_count(), MAX_GLOBAL_ACTIVE_TASKS);

        // 513th global active task from a fresh 9th principal also fails with -32000.
        let ninth = TaskPrincipal::from_node("quota-node-overflow");
        let global_err = quota_mgr
            .try_spawn_for(ninth.clone(), TaskOptions::default(), |_ctx| {
                Box::pin(async move { Ok(CallToolResult::structured(json!({}))) })
            })
            .unwrap_err();
        assert_eq!(global_err.code, ErrorCode(-32000));

        // Cancelling one active task immediately frees both global and per-principal capacity.
        let p0 = TaskPrincipal::from_node("quota-node-0");
        quota_mgr.cancel_task_for(&p0_ids[0], &p0).unwrap();
        for _ in 0..50 {
            if quota_mgr.running_task_count() < MAX_GLOBAL_ACTIVE_TASKS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let reclaimed = quota_mgr
            .try_spawn_for(ninth.clone(), TaskOptions::default(), |_ctx| {
                Box::pin(async move { Ok(CallToolResult::structured(json!({"slot": "freed"}))) })
            })
            .expect("cancelled task must reclaim global active slot");
        assert!(quota_mgr.get_task_for(&reclaimed.task_id, &ninth).is_ok());

        // Terminal retention eviction cap (`MAX_RETAINED_TERMINAL_TASKS = 2048`).
        let retain_mgr = TaskManager::new();
        let fast_poll_opts = TaskOptions {
            poll_interval_ms: Some(1),
            ..Default::default()
        };
        let mut first_terminal_id = String::new();
        for idx in 0..(MAX_RETAINED_TERMINAL_TASKS + 8) {
            let t = retain_mgr
                .try_spawn_for(alice_p.clone(), fast_poll_opts.clone(), |_ctx| {
                    Box::pin(async move { Ok(CallToolResult::structured(json!({}))) })
                })
                .unwrap();
            if idx == 0 {
                first_terminal_id = t.task_id.clone();
            }
            while retain_mgr.running_task_count() > 0 {
                tokio::task::yield_now().await;
            }
        }
        // Trigger sweep after all 2056 tasks have settled: oldest terminal entry must be evicted.
        assert_eq!(retain_mgr.running_task_count(), 0);
        assert_eq!(
            retain_mgr
                .get_task_for(&first_terminal_id, &alice_p)
                .unwrap_err()
                .code,
            ErrorCode::INVALID_PARAMS,
            "excess terminal records beyond MAX_RETAINED_TERMINAL_TASKS must be evicted"
        );

        // 2e. Duplicate inputRequests key reuse, >128 key cap, >1 MiB inputResponses,
        //     `notifications/cancelled` non-cancellation, covert-leakage prohibition,
        //     per-request capability spoofing, and concurrent race invariants (`T4`, `T6`, `T7`, `T8`).
        let svc = GmcpService::open_durable(root.to_path_buf(), home.join("wire-adv-store")).unwrap();

        // Tool 1: attempts to reuse an inputRequests key (`F-NEG-014-04`, `-0350`).
        svc.register_task_tool(
            serde_json::from_value::<Tool>(
                json!({"name": "adv_dup_key", "inputSchema": {"type": "object"}}),
            )
            .unwrap(),
            Arc::new(|_, ctx| {
                Box::pin(async move {
                    let req: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    let _ = ctx.request_input("same-key", req.clone()).await?;
                    let _ = ctx.request_input("same-key", req).await?;
                    Ok(CallToolResult::structured(json!({"unreachable": true})))
                })
            }),
        )
        .unwrap();

        // Tool 2: exceeds MAX_INPUT_REQUESTS_PER_TASK (`128`).
        svc.register_task_tool(
            serde_json::from_value::<Tool>(
                json!({"name": "adv_cap_keys", "inputSchema": {"type": "object"}}),
            )
            .unwrap(),
            Arc::new(|_, ctx| {
                Box::pin(async move {
                    let req: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    for idx in 0..=MAX_INPUT_REQUESTS_PER_TASK {
                        let _ = ctx.request_input(format!("k-{idx}"), req.clone()).await?;
                    }
                    Ok(CallToolResult::structured(json!({"unreachable": true})))
                })
            }),
        )
        .unwrap();

        // Tool 3: protocol failure (`Err(TaskExit::Error)`) vs tool `isError: true` (`-0158`, `-0835`, `-0837`).
        svc.register_task_tool(
            serde_json::from_value::<Tool>(
                json!({"name": "adv_outcomes", "inputSchema": {"type": "object"}}),
            )
            .unwrap(),
            Arc::new(|req, ctx| {
                Box::pin(async move {
                    let mode = req
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("mode"))
                        .and_then(Value::as_str)
                        .unwrap_or("ok");
                    match mode {
                        "rpc_err" => Err(TaskExit::Error(ErrorData::internal_error(
                            "protocol engine fault",
                            Some(json!({"reason": "simulated"})),
                        ))),
                        "tool_err" => Ok(CallToolResult::error(vec![ContentBlock::text(
                            "domain validation error",
                        )])),
                        "hold_for_note_cancel" => {
                            tokio::time::sleep(Duration::from_millis(60)).await;
                            if ctx.is_cancel_requested() {
                                return Err(TaskExit::Cancelled);
                            }
                            Ok(CallToolResult::success(vec![ContentBlock::text(
                                "survived-notifications-cancelled",
                            )]))
                        }
                        _ => Ok(CallToolResult::structured(json!({"ok": true}))),
                    }
                })
            }),
        )
        .unwrap();

        let sub_session = svc.session();
        let open_duplex = |s: GmcpService| async move {
            let (c, srv) = tokio::io::duplex(2 * 1024 * 1024);
            tokio::spawn(async move {
                if let Ok(running) = s.serve(srv).await {
                    let _ = running.waiting().await;
                }
            });
            BufReader::new(c)
        };
        let mut wire = open_duplex(svc.clone()).await;
        let mut sub_wire = open_duplex(sub_session).await;

        async fn send_msg(w: &mut BufReader<tokio::io::DuplexStream>, msg: Value) {
            let bytes = format!("{msg}\n").into_bytes();
            w.get_mut().write_all(&bytes).await.unwrap();
        }
        async fn recv_msg(w: &mut BufReader<tokio::io::DuplexStream>) -> Value {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(10), w.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        }

        // Bootstrap both modern wires.
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": tasks_meta("1")}}),
        )
        .await;
        assert_eq!(recv_msg(&mut wire).await["result"]["resultType"], "complete");
        send_msg(
            &mut sub_wire,
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": tasks_meta("s1")}}),
        )
        .await;
        assert_eq!(recv_msg(&mut sub_wire).await["result"]["resultType"], "complete");

        // Retired 2025-11-25 `tasks/list` and `tasks/result` fail closed with `-32601` (`F-NEG-014-07`).
        for (id, retired) in [(2, "tasks/list"), (3, "tasks/result")] {
            send_msg(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": id, "method": retired, "params": {"_meta": tasks_meta(&id.to_string())}}),
            )
            .await;
            assert_eq!(recv_msg(&mut wire).await["error"]["code"], -32601);
        }

        // Duplicate `inputRequests` key reuse fails closed -> `failed` with diagnostic `statusMessage` (`F-NEG-014-04`).
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "adv_dup_key", "arguments": {}, "_meta": tasks_meta("4")}}),
        )
        .await;
        let dup_tid = recv_msg(&mut wire).await["result"]["taskId"]
            .as_str()
            .unwrap()
            .to_string();
        for _ in 0..40 {
            send_msg(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": 40, "method": "tasks/get", "params": {"taskId": dup_tid, "_meta": tasks_meta("40")}}),
            )
            .await;
            let snap = recv_msg(&mut wire).await;
            if snap["result"]["status"] == "input_required" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Oversized (> 1 MiB) and non-object `inputResponses` fail with `-32602` without corrupting pending key!
        let huge_str = "x".repeat(MAX_INPUT_RESPONSE_BYTES + 128);
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 41, "method": "tasks/update", "params": {"taskId": dup_tid, "inputResponses": {"same-key": huge_str}, "_meta": tasks_meta("41")}}),
        )
        .await;
        assert_eq!(recv_msg(&mut wire).await["error"]["code"], -32602);
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 42, "method": "tasks/update", "params": {"taskId": dup_tid, "inputResponses": ["not-an-object"], "_meta": tasks_meta("42")}}),
        )
        .await;
        assert_eq!(recv_msg(&mut wire).await["error"]["code"], -32602);

        // Answer `"same-key"` once -> server attempts to reuse `"same-key"` and fails closed (`failed`, `-32603`).
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 43, "method": "tasks/update", "params": {"taskId": dup_tid, "inputResponses": {"same-key": {"roots": []}}, "_meta": tasks_meta("43")}}),
        )
        .await;
        assert_eq!(recv_msg(&mut wire).await["result"]["resultType"], "complete");
        let mut dup_failed = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            send_msg(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": 44, "method": "tasks/get", "params": {"taskId": dup_tid, "_meta": tasks_meta("44")}}),
            )
            .await;
            let snap = recv_msg(&mut wire).await;
            if snap["result"]["status"] == "failed" {
                assert_eq!(snap["result"]["error"]["code"], -32603);
                assert!(
                    snap["result"]["statusMessage"]
                        .as_str()
                        .is_some_and(|m| m.contains("already used")),
                    "expected diagnostic statusMessage on key reuse: {snap}"
                );
                dup_failed = true;
                break;
            }
        }
        assert!(dup_failed, "server duplicate inputRequests key must fail closed");

        // `MAX_INPUT_REQUESTS_PER_TASK = 128` cap enforcement: 129th `request_input` fails task with `-32602`.
        let mut cap_events = svc.tasks.subscribe_events();
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 50, "method": "tools/call", "params": {"name": "adv_cap_keys", "arguments": {}, "_meta": tasks_meta("50")}}),
        )
        .await;
        let cap_tid = recv_msg(&mut wire).await["result"]["taskId"]
            .as_str()
            .unwrap()
            .to_string();
        let mut cap_settled_failed = false;
        while let Ok(Ok((_, ev))) =
            tokio::time::timeout(Duration::from_secs(5), cap_events.recv()).await
        {
            if ev.task.task_id != cap_tid {
                continue;
            }
            if let susi_gmcp::mcp::model::TaskPayload::InputRequired { input_requests } =
                &ev.payload
            {
                let replies: Vec<(String, Value)> = input_requests
                    .keys()
                    .map(|k| (k.clone(), json!({"roots": []})))
                    .collect();
                svc.tasks.update_task(&cap_tid, replies).unwrap();
            } else if ev.status() == SrvStatus::Failed {
                let v = serde_json::to_value(&ev).unwrap();
                assert_eq!(v["error"]["code"], -32602);
                assert!(v["statusMessage"]
                    .as_str()
                    .is_some_and(|m| m.contains("max inputRequests")));
                cap_settled_failed = true;
                break;
            }
        }
        assert!(
            cap_settled_failed,
            "129th inputRequest must settle task to Failed (-32602)"
        );

        // `isError: true` (`completed` + `result`) vs protocol `ErrorData` (`failed` + `error` + `statusMessage`) (`-0835`, `-0837`).
        for (req_id, mode, exp_status) in [
            (60, "tool_err", "completed"),
            (61, "rpc_err", "failed"),
        ] {
            send_msg(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": req_id, "method": "tools/call", "params": {"name": "adv_outcomes", "arguments": {"mode": mode}, "_meta": tasks_meta(&req_id.to_string())}}),
            )
            .await;
            let tid = recv_msg(&mut wire).await["result"]["taskId"]
                .as_str()
                .unwrap()
                .to_string();
            let mut verified = false;
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                send_msg(
                    &mut wire,
                    json!({"jsonrpc": "2.0", "id": req_id + 100, "method": "tasks/get", "params": {"taskId": tid, "_meta": tasks_meta("g")}}),
                )
                .await;
                let snap = recv_msg(&mut wire).await;
                if snap["result"]["status"] == exp_status {
                    if mode == "tool_err" {
                        assert_eq!(snap["result"]["result"]["isError"], true);
                        assert!(snap["result"].get("error").is_none());
                    } else {
                        assert_eq!(snap["result"]["error"]["code"], -32603);
                        assert_eq!(snap["result"]["statusMessage"], "protocol engine fault");
                        assert!(snap["result"].get("result").is_none());
                    }
                    verified = true;
                    break;
                }
            }
            assert!(verified, "mode {mode} must settle to {exp_status}");
        }

        // `notifications/cancelled` MUST NOT cancel a spawned durable task (`-0385`, `F-NEG-014-05`),
        // AND per-request capability omission on the SAME connection MUST fail `-32021` (`-0061`, `F-NEG-014-01`).
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 70, "method": "tools/call", "params": {"name": "adv_outcomes", "arguments": {"mode": "hold_for_note_cancel"}, "_meta": tasks_meta("70")}}),
        )
        .await;
        let held_tid = recv_msg(&mut wire).await["result"]["taskId"]
            .as_str()
            .unwrap()
            .to_string();

        // Subscribe to `held_tid` on `sub_wire` to verify zero covert leakage too.
        send_msg(
            &mut sub_wire,
            json!({"jsonrpc": "2.0", "id": 2, "method": "subscriptions/listen", "params": {"notifications": {"taskIds": [held_tid]}, "_meta": tasks_meta("s2")}}),
        )
        .await;
        assert_eq!(
            recv_msg(&mut sub_wire).await["method"],
            "notifications/subscriptions/acknowledged"
        );

        // Send `notifications/cancelled` referencing both original requestId 70 and taskId `held_tid`.
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 70, "reason": "misuse"}}),
        )
        .await;
        send_msg(
            &mut wire,
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": held_tid, "reason": "misuse"}}),
        )
        .await;

        // Immediately issue requests WITHOUT `io.modelcontextprotocol/tasks` on the SAME `wire` (`-0061`, `-0798`, `-0799`).
        for (id, method, mut p) in [
            (71, "tools/call", json!({"name": "adv_outcomes", "arguments": {}})),
            (72, "tasks/get", json!({"taskId": held_tid})),
            (73, "tasks/update", json!({"taskId": held_tid, "inputResponses": {}})),
            (74, "tasks/cancel", json!({"taskId": held_tid})),
            (
                75,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": [held_tid]}}),
            ),
        ] {
            p["_meta"] = plain_meta(&id.to_string());
            send_msg(&mut wire, json!({"jsonrpc": "2.0", "id": id, "method": method, "params": p})).await;
            let err = recv_msg(&mut wire).await;
            assert_eq!(
                err["error"]["code"], -32021,
                "{method} without per-request tasks capability must fail -32021: {err}"
            );
            assert!(
                err["error"]["data"]["requiredCapabilities"]["extensions"][TASKS_EXTENSION_ID]
                    .is_object(),
                "missing capability error must name io.modelcontextprotocol/tasks: {err}"
            );
        }

        // Confirm `held_tid` was NOT cancelled by `notifications/cancelled` and reaches `completed` cleanly!
        let mut held_completed = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            send_msg(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": 76, "method": "tasks/get", "params": {"taskId": held_tid, "_meta": tasks_meta("76")}}),
            )
            .await;
            let snap = recv_msg(&mut wire).await;
            if snap["result"]["status"] == "completed" {
                assert_eq!(
                    snap["result"]["result"]["content"][0]["text"],
                    "survived-notifications-cancelled"
                );
                held_completed = true;
                break;
            }
        }
        assert!(
            held_completed,
            "notifications/cancelled must never cancel a durable spawned task"
        );

        let sub_note = recv_msg(&mut sub_wire).await;
        assert_eq!(sub_note["method"], "notifications/tasks");
        assert_eq!(sub_note["params"]["status"], "completed");

        // Subscription session cap (`MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION = 16`) & >64 taskIds cap (`T7`).
        let over_ids: Vec<String> = (0..=MAX_SUBSCRIPTION_TASK_IDS)
            .map(|i| format!("tid-{i}"))
            .collect();
        send_msg(
            &mut sub_wire,
            json!({"jsonrpc": "2.0", "id": 80, "method": "subscriptions/listen", "params": {"notifications": {"taskIds": over_ids}, "_meta": tasks_meta("s80")}}),
        )
        .await;
        assert_eq!(recv_msg(&mut sub_wire).await["error"]["code"], -32602);

        // `sub_wire` already holds 1 active subscription (`id: 2`); open 15 more (`3..=17`), then prove 17th fails closed!
        for sub_idx in 0..(MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION - 1) {
            let rid = 100 + sub_idx as i64;
            send_msg(
                &mut sub_wire,
                json!({"jsonrpc": "2.0", "id": rid, "method": "subscriptions/listen", "params": {"notifications": {"toolsListChanged": true}, "_meta": tasks_meta(&rid.to_string())}}),
            )
            .await;
            assert_eq!(
                recv_msg(&mut sub_wire).await["method"],
                "notifications/subscriptions/acknowledged"
            );
        }
        send_msg(
            &mut sub_wire,
            json!({"jsonrpc": "2.0", "id": 199, "method": "subscriptions/listen", "params": {"notifications": {"toolsListChanged": true}, "_meta": tasks_meta("199")}}),
        )
        .await;
        let overflow_sub = recv_msg(&mut sub_wire).await;
        assert!(
            overflow_sub.get("error").is_some(),
            "17th concurrent subscription on same session must fail closed: {overflow_sub}"
        );

        // Cancel one active subscription via `notifications/cancelled` -> frees slot (with zero response emitted for cancelled request) so next subscription succeeds!
        send_msg(
            &mut sub_wire,
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 100}}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        send_msg(
            &mut sub_wire,
            json!({"jsonrpc": "2.0", "id": 200, "method": "subscriptions/listen", "params": {"notifications": {"toolsListChanged": true}, "_meta": tasks_meta("200")}}),
        )
        .await;
        assert_eq!(
            recv_msg(&mut sub_wire).await["method"],
            "notifications/subscriptions/acknowledged"
        );

        // 2f. Deterministic multi-task concurrency race (`tasks/cancel` + `tasks/update` + completion) (`T6`).
        let race_mgr = TaskManager::new();
        for round in 0..16 {
            let task = race_mgr
                .try_spawn_for(alice_p.clone(), fast_poll_opts.clone(), move |ctx| {
                    Box::pin(async move {
                        if round % 2 == 0 {
                            let req: InputRequest =
                                serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                            let _ = ctx.request_input("race-k", req).await?;
                        } else {
                            tokio::task::yield_now().await;
                        }
                        if ctx.is_cancel_requested() {
                            return Err(TaskExit::Cancelled);
                        }
                        Ok(CallToolResult::structured(json!({"round": round})))
                    })
                })
                .unwrap();
            let tid = task.task_id.clone();
            let (m1, p1, id1) = (race_mgr.clone(), alice_p.clone(), tid.clone());
            let (m2, p2, id2) = (race_mgr.clone(), alice_p.clone(), tid.clone());
            let j1 = tokio::spawn(async move {
                let _ = m1.update_task_for(&id1, &p1, vec![("race-k".into(), json!({"roots": []}))]);
            });
            let j2 = tokio::spawn(async move {
                let _ = m2.cancel_task_for(&id2, &p2);
            });
            let _ = tokio::join!(j1, j2);
            while race_mgr.running_task_count() > 0 {
                tokio::task::yield_now().await;
            }
            let settled_before = serde_json::to_value(
                race_mgr.get_task_for(&tid, &alice_p).unwrap(),
            )
            .unwrap();
            assert!(
                matches!(
                    settled_before["status"].as_str(),
                    Some("completed" | "cancelled")
                ),
                "unexpected raced terminal status: {settled_before}"
            );
            // Late cancel and late update MUST fail `-32602` and leave terminal snapshot immutable.
            assert_eq!(
                race_mgr.cancel_task_for(&tid, &alice_p).unwrap_err().code,
                ErrorCode::INVALID_PARAMS
            );
            assert_eq!(
                race_mgr
                    .update_task_for(&tid, &alice_p, vec![("late".into(), json!(1))])
                    .unwrap_err()
                    .code,
                ErrorCode::INVALID_PARAMS
            );
            let settled_after = serde_json::to_value(
                race_mgr.get_task_for(&tid, &alice_p).unwrap(),
            )
            .unwrap();
            assert_eq!(
                settled_before, settled_after,
                "terminal payload mutated after late cancel/update"
            );
        }
        assert_eq!(race_mgr.running_task_count(), 0);
    });

    // ── 3. T3, T7, T8: Live HTTP `GmcpServer` NetGuard, `Mcp-Name`, 413 DoS & Cross-Bearer Hijack ──
    let _runtime = adapter_runtime(&home);
    struct AdvList;
    impl PlaneHandler for AdvList {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Ok(json!([{"name": "adv_http_echo", "description": "Adversarial HTTP task probe"}]))
        }
    }
    struct AdvExists;
    impl PlaneHandler for AdvExists {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            Ok(json!({ "exists": name == "adv_http_echo" }))
        }
    }
    struct AdvExec;
    impl PlaneHandler for AdvExec {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            with_watched(&payload, || Ok(json!({ "text": "adv-http-ok" })))
        }
    }
    struct RestoreAdvPlane;
    impl Drop for RestoreAdvPlane {
        fn drop(&mut self) {
            let bus = PlaneBus::global();
            bus.unregister(topics::TOOLS_LIST);
            bus.unregister(topics::TOOLS_EXISTS);
            bus.unregister(topics::TOOLS_EXECUTE);
            susi_tools::plane_handler::register();
        }
    }
    let _restore_adv_plane = RestoreAdvPlane;
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(AdvList));
    PlaneBus::global().register(topics::TOOLS_EXISTS, Arc::new(AdvExists));
    PlaneBus::global().register(topics::TOOLS_EXECUTE, Arc::new(AdvExec));

    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let tok_alice = "adv-http-alice-token";
    let tok_bob = "adv-http-bob-token";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_alice}\n")).unwrap();
    std::fs::write(
        cfg_dir.join("config.json"),
        r#"{"max_rpc_body_bytes": 4096}"#,
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let srv_root = root.to_path_buf();
    let _srv = std::thread::spawn(move || {
        GmcpServer::start_http_server(srv_root, vec![listener], None, false);
    });
    for _ in 0..100 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    fn read_http_msg(s: &mut TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let mut target_len = None;
        loop {
            match s.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if target_len.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let hdr_end = pos + 4;
                            let hdr_str = String::from_utf8_lossy(&buf[..hdr_end]);
                            let cl = hdr_str
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.trim()
                                        .eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            target_len = Some(hdr_end + cl);
                        }
                    }
                    if target_len.is_some_and(|len| buf.len() >= len) {
                        break;
                    }
                }
            }
        }
        buf
    }

    let raw_http = |hdrs: &[(&str, &str)], body: &str| -> (u16, String) {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if !hdrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: {addr}\r\n"));
        }
        for (k, v) in hdrs {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        s.write_all(req.as_bytes()).unwrap();
        let resp = String::from_utf8_lossy(&read_http_msg(&mut s)).into_owned();
        let code = resp
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        (code, resp)
    };

    let sample_get_body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tasks/get", "params": {"taskId": "tid-http-1", "_meta": tasks_meta("h1")}})
    .to_string();
    let auth_hdr = format!("Bearer {tok_alice}");

    // NetGuard: unauthenticated (`401`), wrong token (`401`), hostile Origin (`403`).
    assert_eq!(
        raw_http(
            &[
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
                ("Mcp-Method", "tasks/get"),
                ("Mcp-Name", "tid-http-1"),
            ],
            &sample_get_body,
        )
        .0,
        401
    );
    assert_eq!(
        raw_http(
            &[
                ("Authorization", "Bearer wrong-token"),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
                ("Mcp-Method", "tasks/get"),
                ("Mcp-Name", "tid-http-1"),
            ],
            &sample_get_body,
        )
        .0,
        401
    );
    assert_eq!(
        raw_http(
            &[
                ("Authorization", &auth_hdr),
                ("Origin", "https://evil.attacker.invalid"),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
                ("Mcp-Method", "tasks/get"),
                ("Mcp-Name", "tid-http-1"),
            ],
            &sample_get_body,
        )
        .0,
        403
    );

    // Oversized HTTP task payload (> 4096 bytes) rejected with `413 Payload Too Large` (`T7`).
    let big_http = json!({ "jsonrpc": "2.0", "id": 2, "method": "tasks/get", "params": {"taskId": "x".repeat(5000), "_meta": tasks_meta("h2")}})
    .to_string();
    assert_eq!(
        raw_http(
            &[
                ("Authorization", &auth_hdr),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
                ("Mcp-Method", "tasks/get"),
            ],
            &big_http,
        )
        .0,
        413
    );

    // Malformed JSON (`415`), JSON-RPC batch array (`400/-32600`), and bare no-`id` `tasks/get` (`400/-32600`).
    let (batch_code, batch_resp) = raw_http(
        &[
            ("Authorization", &auth_hdr),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        r#"[{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{"taskId":"t"}}]"#,
    );
    assert_eq!(batch_code, 400, "{batch_resp}");
    assert!(batch_resp.contains("-32600"), "{batch_resp}");

    let (noid_code, noid_resp) = raw_http(
        &[
            ("Authorization", &auth_hdr),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tasks/get"),
            ("Mcp-Name", "tid-http-1"),
        ],
        r#"{"jsonrpc":"2.0","method":"tasks/get","params":{"taskId":"tid-http-1"}}"#,
    );
    assert_eq!(noid_code, 400, "{noid_resp}");
    assert!(noid_resp.contains("-32600"), "{noid_resp}");

    // `Mcp-Name` & `Mcp-Method` validation matrix (`F-NEG-014-06`, `-0511`):
    // missing Mcp-Name, mismatched Mcp-Name, duplicate Mcp-Name, invalid Base64 Mcp-Name, mismatched Mcp-Method -> `400 / -32020`.
    for (method, body_val) in [
        (
            "tasks/get",
            json!({"jsonrpc": "2.0", "id": 10, "method": "tasks/get", "params": {"taskId": "tid-http-1", "_meta": tasks_meta("m1")}}),
        ),
        (
            "tasks/update",
            json!({"jsonrpc": "2.0", "id": 11, "method": "tasks/update", "params": {"taskId": "tid-http-1", "inputResponses": {}, "_meta": tasks_meta("m2")}}),
        ),
        (
            "tasks/cancel",
            json!({"jsonrpc": "2.0", "id": 12, "method": "tasks/cancel", "params": {"taskId": "tid-http-1", "_meta": tasks_meta("m3")}}),
        ),
    ] {
        let body = body_val.to_string();
        for bad_hdrs in [
            vec![("Mcp-Method", method)],
            vec![("Mcp-Method", method), ("Mcp-Name", "wrong-task-id")],
            vec![
                ("Mcp-Method", method),
                ("Mcp-Name", "tid-http-1"),
                ("Mcp-Name", "tid-http-1"),
            ],
            vec![("Mcp-Method", method), ("Mcp-Name", "=?base64?***?=")],
            vec![("Mcp-Method", "tools/call"), ("Mcp-Name", "tid-http-1")],
        ] {
            let mut hdrs = vec![
                ("Authorization", auth_hdr.as_str()),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ];
            hdrs.extend(bad_hdrs);
            let (code, resp) = raw_http(&hdrs, &body);
            assert_eq!(
                code, 400,
                "expected 400 header mismatch on {method}: {resp}"
            );
            assert!(
                resp.contains("-32020"),
                "expected -32020 HEADER_MISMATCH on {method}: {resp}"
            );
        }
    }

    // Spawn server-directed task (`adv_http_echo`) as Alice over HTTP, then rotate token to Bob
    // and prove Bob cannot read, update, or cancel Alice's task even with spoofed `_meta` fields (`T2`).
    let mut alice_http =
        Session::open_2026_07_28(&addr, Some(tok_alice), Duration::from_secs(15)).unwrap();
    let alice_out = alice_http
        .call_tool_with_tasks("adv_http_echo", &json!({}), true, &mut |_| {})
        .expect("Alice adv_http_echo task must spawn");
    let CallToolOutcome::Task(alice_handle) = alice_out else {
        panic!("expected server-directed CreateTaskResult for adv_http_echo");
    };
    let alice_done = alice_http
        .poll_task_until_done(&alice_handle, Some(Duration::from_secs(15)), |_| {
            Ok(json!({}))
        })
        .expect("Alice must poll own task to completion");
    assert_eq!(alice_done.handle.status, TaskStatus::Completed);

    // Tight-loop HTTP `tasks/get` rate-limit defense (`T7`, `-0308`):
    let mut saw_http_ratelimit = false;
    for _ in 0..32 {
        if let Err(WireError::Rpc(-32000, msg)) = alice_http.get_task(&alice_handle.task_id) {
            assert!(msg.contains("pollIntervalMs"), "{msg}");
            saw_http_ratelimit = true;
            break;
        }
    }
    assert!(
        saw_http_ratelimit,
        "rapid HTTP tasks/get burst must trigger -32000 rate limit"
    );

    std::fs::write(cfg_dir.join("api_token"), format!("{tok_bob}\n")).unwrap();
    let mut bob_http =
        Session::open_2026_07_28(&addr, Some(tok_bob), Duration::from_secs(15)).unwrap();
    assert!(matches!(
        bob_http.get_task(&alice_handle.task_id),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        bob_http.update_task(&alice_handle.task_id, &json!({"spoof": true})),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        bob_http.cancel_task(&alice_handle.task_id),
        Err(WireError::Rpc(-32602, _))
    ));

    // Spoofed `_meta.principal` / `_meta.owner` on raw Bob HTTP request still rejected `-32602`!
    let bob_auth = format!("Bearer {tok_bob}");
    let mut spoof_meta = tasks_meta("spoof-1");
    spoof_meta["principal"] = json!(alice_p.as_str());
    spoof_meta["owner"] = json!(alice_p.as_str());
    let spoof_body = json!({ "jsonrpc": "2.0", "id": 99, "method": "tasks/get", "params": {"taskId": alice_handle.task_id, "_meta": spoof_meta}})
    .to_string();
    let (_, spoof_resp) = raw_http(
        &[
            ("Authorization", &bob_auth),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tasks/get"),
            ("Mcp-Name", &alice_handle.task_id),
        ],
        &spoof_body,
    );
    assert!(
        spoof_resp.contains("-32602"),
        "spoofed _meta owner/principal must never bypass auth binding: {spoof_resp}"
    );

    // ── 4. T4, T5, T8: Client-side `susi-wire-client` adversarial & corruption defenses ──
    // 4a. `PersistedTaskHandleStore` corruption matrix (`F-NEG-014-08`).
    let client_store_path = home.join("client-handles.json");
    let mut cstore = PersistedTaskHandleStore::open(&client_store_path).unwrap();
    cstore
        .save(PersistedTaskHandle::from_handle(
            &addr,
            PROTOCOL_VERSION_2026_07_28,
            &alice_handle,
        ))
        .unwrap();
    let valid_client_bytes = std::fs::read(&client_store_path).unwrap();
    drop(cstore);
    for (label, bad) in [
        ("broken-json", b"{broken".to_vec()),
        (
            "bad-version",
            serde_json::to_vec(&json!({"version": 2, "handles": []})).unwrap(),
        ),
        (
            "missing-handles",
            serde_json::to_vec(&json!({"version": 1})).unwrap(),
        ),
        ("duplicate-handle", {
            let mut v: Value = serde_json::from_slice(&valid_client_bytes).unwrap();
            let first = v["handles"][0].clone();
            v["handles"].as_array_mut().unwrap().push(first);
            serde_json::to_vec(&v).unwrap()
        }),
        ("unknown-status", {
            let mut v: Value = serde_json::from_slice(&valid_client_bytes).unwrap();
            v["handles"][0]["status"] = json!("bogus_state");
            serde_json::to_vec(&v).unwrap()
        }),
    ] {
        std::fs::write(&client_store_path, bad).unwrap();
        assert!(
            matches!(
                PersistedTaskHandleStore::open(&client_store_path),
                Err(WireError::Protocol(_))
            ),
            "PersistedTaskHandleStore::open must fail closed on {label}"
        );
    }

    // 4b. Client MRTR `inputRequests` validation & hostile mock-server `resultType` forgery (`T4`, `T8`).
    for bad_get in [
        // Missing resultType: "complete" on tasks/get (`-0338`)
        json!({"resultType": "task", "taskId": "t1", "status": "working", "createdAt": "t0", "lastUpdatedAt": "t0"}),
        // Disallowed MRTR method (`-0348`, `-0905`)
        json!({"resultType": "complete", "taskId": "t1", "status": "input_required", "createdAt": "t0", "lastUpdatedAt": "t0", "inputRequests": {"k": {"method": "tools/call", "params": {}}}}),
        // Empty MRTR key
        json!({"resultType": "complete", "taskId": "t1", "status": "input_required", "createdAt": "t0", "lastUpdatedAt": "t0", "inputRequests": {"": {"method": "roots/list"}}}),
        // Non-object params on elicitation/create
        json!({"resultType": "complete", "taskId": "t1", "status": "input_required", "createdAt": "t0", "lastUpdatedAt": "t0", "inputRequests": {"k": {"method": "elicitation/create", "params": "bad"}}}),
    ] {
        assert!(
            matches!(
                TaskSnapshot::from_get_result(&bad_get),
                Err(WireError::Protocol(_))
            ),
            "TaskSnapshot::from_get_result must reject hostile payload: {bad_get}"
        );
    }

    // Scripted hostile HTTP server testing `Session` against forged `CreateTaskResult`,
    // forged `resultType` on `tasks/update` & `tasks/cancel`, and CRLF/NUL `taskId` injection.
    let mock_l = TcpListener::bind("127.0.0.1:0").unwrap();
    let mock_addr = format!("127.0.0.1:{}", mock_l.local_addr().unwrap().port());
    let mock_replies = vec![
        // 1. server/discover
        r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"extensions":{"io.modelcontextprotocol/tasks":{}}}}}"#,
        // 2. Forged CreateTaskResult when client called `call_tool` (tasks NOT declared!) (`-0061`)
        r#"{"jsonrpc":"2.0","id":2,"result":{"resultType":"task","taskId":"forged-1","status":"working","createdAt":"t0","lastUpdatedAt":"t0"}}"#,
        // 3. Forged CreateTaskResult on `tools/list` (`-0061`, `-0102`)
        r#"{"jsonrpc":"2.0","id":3,"result":{"resultType":"task","taskId":"forged-2","status":"working","createdAt":"t0","lastUpdatedAt":"t0"}}"#,
        // 4. Missing `resultType: "complete"` on `tasks/update` (`-0381`)
        r#"{"jsonrpc":"2.0","id":4,"result":{}}"#,
        // 5. Forged `resultType: "input_required"` on `tasks/cancel` (`-0410`)
        r#"{"jsonrpc":"2.0","id":5,"result":{"resultType":"input_required"}}"#,
        // 6. `input_required` snapshot with empty `inputRequests` rejected by `poll_task_until_done`
        r#"{"jsonrpc":"2.0","id":6,"result":{"resultType":"complete","taskId":"t-empty-ir","status":"input_required","createdAt":"t0","lastUpdatedAt":"t0","inputRequests":{}}}"#,
    ];
    let mock_srv = std::thread::spawn(move || {
        for body in mock_replies {
            let (mut stream, _) = mock_l.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let _ = read_http_msg(&mut stream);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.read(&mut [0u8; 16]);
        }
    });

    let mut mock_sess = Session::open_2026_07_28(&mock_addr, None, Duration::from_secs(5)).unwrap();
    // CRLF/NUL/empty taskId rejected locally before touching the socket!
    for bad_id in ["", "id\r\nInjected: 1", "id\u{0000}nul"] {
        assert!(matches!(
            mock_sess.get_task(bad_id),
            Err(WireError::Protocol(_))
        ));
        assert!(matches!(
            mock_sess.update_task(bad_id, &json!({})),
            Err(WireError::Protocol(_))
        ));
        assert!(matches!(
            mock_sess.cancel_task(bad_id),
            Err(WireError::Protocol(_))
        ));
    }
    assert!(matches!(
        mock_sess.call_tool("echo", &json!({}), None, &mut |_| {}),
        Err(WireError::Protocol(_))
    ));
    assert!(matches!(
        mock_sess.request("tools/list", &json!({}), &mut |_| {}),
        Err(WireError::Protocol(_))
    ));
    assert!(matches!(
        mock_sess.update_task("valid-tid", &json!({"k": 1})),
        Err(WireError::Protocol(_))
    ));
    assert!(matches!(
        mock_sess.cancel_task("valid-tid"),
        Err(WireError::Protocol(_))
    ));
    let empty_ir_handle = TaskHandle {
        task_id: "t-empty-ir".into(),
        status: TaskStatus::Working,
        status_message: None,
        created_at: "t0".into(),
        last_updated_at: "t0".into(),
        ttl_ms: Some(5000),
        poll_interval_ms: Some(10),
    };
    assert!(matches!(
        mock_sess.poll_task_until_done(&empty_ir_handle, None, |_| Ok(json!({"x": 1}))),
        Err(WireError::Protocol(_))
    ));
    mock_srv.join().unwrap();

    // Wire gate sanity on hostile task frames (`classify_envelope` / `classify_line` / `http_envelope_rejection`).
    let mut gate_era = EraState::default();
    for raw in [
        br#"{"jsonrpc":"2.0","id":null,"method":"tasks/cancel","params":{"taskId":"t"}}"#
            .as_slice(),
        br#"{"jsonrpc":"2.0","method":"tasks/cancel","params":{"taskId":"t"}}"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{"taskId":"t"}}]"#.as_slice(),
    ] {
        assert!(matches!(
            classify_envelope(raw),
            GateVerdict::Reject { code: -32600, .. }
        ));
        assert!(matches!(
            classify_line(raw, &mut gate_era),
            GateVerdict::Reject { code: -32600, .. }
        ));
        assert_eq!(
            http_envelope_rejection(raw).and_then(|v| v["error"]["code"].as_i64()),
            Some(-32600)
        );
    }

    let _ = std::fs::remove_dir_all(&home);
}
