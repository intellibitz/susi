//! Extracted unit tests for [`crate::tools::CoreTools`] and core tool helpers.

#[cfg(test)]
mod sovereign_dashboard_tests {
    use crate::tools::CoreTools;

    #[test]
    fn dashboard_counts_real_event_types_only() {
        let ws = std::env::temp_dir().join(format!("susi-dash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        let entry =
            |t: &str, d: &str| serde_json::json!({ "type": t, "details": d, "ts": 1 }).to_string();
        let log = [
            entry("LATENCY_VIOLATION", "reflex 3ms"),
            entry("LATENCY_VIOLATION", "reflex 4ms"),
            entry("MISSION_START", "summarize SELF_HEALING notes"),
        ]
        .join("\n");
        std::fs::write(ws.join(".susi/audit.log"), log).unwrap();
        let report = CoreTools::sovereign_dashboard(&serde_json::json!({}), &ws).unwrap();
        assert!(report.contains("**LATENCY_VIOLATION**: 2"), "{report}");
        assert!(report.contains("**MISSION_START**: 1"), "{report}");
        assert!(!report.contains("Self-Heals"), "{report}");
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(test)]
mod exec_pipe_tests {
    use crate::susi_core::task_manager::SwarmTaskManager;
    use crate::tools::core::drain_exec_pipe;
    use std::sync::atomic::Ordering;

    #[test]
    fn exec_pipe_progress_comes_only_from_output() {
        let manager = SwarmTaskManager {
            tasks: std::sync::RwLock::new(std::collections::HashMap::new()),
            cancel_map: std::sync::RwLock::new(std::collections::HashMap::new()),
            pause_map: std::sync::RwLock::new(std::collections::HashMap::new()),
            cancelled_scopes: std::sync::RwLock::new(std::collections::HashSet::new()),
        };
        let silent = manager.register_task("exec_pipe_test", "silent");
        let out = drain_exec_pipe(Some(std::io::Cursor::new(Vec::new())), silent.clone());
        assert!(out.join().unwrap().is_empty());
        assert_eq!(silent.progress_count.load(Ordering::Acquire), 0);

        let chatty = manager.register_task("exec_pipe_test", "chatty");
        let out = drain_exec_pipe(
            Some(std::io::Cursor::new(b"hello".to_vec())),
            chatty.clone(),
        );
        assert_eq!(out.join().unwrap(), b"hello");
        assert!(chatty.progress_count.load(Ordering::Acquire) >= 1);
        silent.mark_failed("done");
        chatty.mark_failed("done");
    }
}

#[cfg(test)]
mod remote_result_tests {
    use crate::tools::core::remote_result_is_error;

    #[test]
    fn mcp_protocol_error_text_is_not_treated_as_tool_output() {
        // The observed live leak: a -32603 surfaced through Ok(String).
        assert!(remote_result_is_error(
            "Protocol Error: Mcp error: -32603: Unknown tool: reason"
        ));
        assert!(remote_result_is_error("[FAIL] MCP: timeout"));
        assert!(remote_result_is_error("MCP Error: tool crashed"));
        assert!(!remote_result_is_error("the capital of France is Paris"));
        assert!(!remote_result_is_error(
            "Much later in the text an error: is mentioned"
        ));
    }
}

#[cfg(test)]
mod shared_runtime_tests {
    use crate::tools::CoreTools;

    #[test]
    fn test_shared_runtime_succeeds_and_is_reused() {
        let rt1 = CoreTools::shared_runtime().expect("runtime should build on a healthy host");
        let val = rt1.block_on(async { 1 + 1 });
        assert_eq!(val, 2);

        // OnceLock caches the Result itself, so a second call returns the
        // same underlying runtime rather than rebuilding one.
        let rt2 = CoreTools::shared_runtime().expect("cached runtime should still be Ok");
        assert!(std::ptr::eq(rt1, rt2));
    }
}

#[cfg(test)]
mod inference_routing_tool_tests {
    use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use crate::tools::CoreTools;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    /// The routing plan echoes the constraints it was asked to plan for.
    struct EchoPlan;
    impl PlaneHandler for EchoPlan {
        fn handle(
            &self,
            _topic: &str,
            payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            Ok(json!({ "asked": payload }))
        }
    }

    /// Placement constraints are validated, never defaulted: the rules the
    /// former HTTP placement query enforced hold on this tool.
    #[test]
    fn inference_placement_accepts_constraints_and_refuses_malformed_ones() {
        let _isolated = crate::test_isolation::enter_isolated_home();
        PlaneBus::global().register(topics::GEMI_ROUTING_PLAN, Arc::new(EchoPlan));
        let ws = Path::new(".");
        let planned: serde_json::Value = serde_json::from_str(
            &CoreTools::inference_placement(
                &json!({"requires": "vision", "max_cost": 0.01, "allow_cloud": false}),
                ws,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            planned["asked"],
            json!({"requires": "vision", "max_cost": 0.01, "allow_cloud": false})
        );
        let defaults: serde_json::Value =
            serde_json::from_str(&CoreTools::inference_placement(&json!({}), ws).unwrap()).unwrap();
        assert_eq!(
            defaults["asked"],
            json!({"requires": null, "max_cost": null, "allow_cloud": true})
        );
        for bad in [
            json!({"requires": "telepathy"}),
            json!({"requires": "vision input"}),
            json!({"requires": 7}),
            json!({"max_cost": -1.0}),
            json!({"max_cost": "NaN"}),
            json!({"allow_cloud": "no"}),
            json!("requires=vision"),
        ] {
            assert!(
                CoreTools::inference_placement(&bad, ws).is_err(),
                "{bad} must be refused"
            );
        }
    }

    /// The provider id rules the former HTTP provider reset enforced. Each
    /// input is refused before the GEMI plane is asked.
    #[test]
    fn inference_reset_provider_refuses_missing_or_malformed_ids() {
        let ws = Path::new(".");
        for bad in [
            json!({}),
            json!({"provider": ""}),
            json!({"provider": "bad id"}),
            json!({"provider": 7}),
            json!("not-an-object"),
        ] {
            let error = CoreTools::inference_reset_provider(&bad, ws).unwrap_err();
            assert!(
                error.to_string().contains("provider is required"),
                "{bad}: {error}"
            );
        }
    }
}

#[cfg(test)]
mod unwired_governance_tests {
    use crate::tools::CoreTools;
    use std::path::Path;

    // With no EngineHooks wired, action-capable tools must fail closed — an
    // unaudited exec/reason path must never reach process execution or the
    // model. Real rejection semantics are covered by wired integration tests
    // in `tests/integration_tests.rs`.

    /// The bus rendezvous is cross-process (`<cache>/bus/*/`), so a sibling
    /// test process — or a live daemon on this host — could satisfy the
    /// `gawd.*` topics and defeat the "unwired" premise. Point the cache dir
    /// at a private temp root before the `IpcPlaneBus` binds
    /// (`XDG_CACHE_HOME` on Linux, `LOCALAPPDATA` on Windows). Under
    /// nextest each test is its own process; under `cargo test` the env swap
    /// races with peers but only shrinks the visible rendezvous, which is
    /// the conservative direction for a fail-closed assertion.
    pub(super) fn isolate_bus_root() {
        // Hold the shared env lock while mutating: `SUSI_XDG` flips the whole
        // config-dir resolution mode, and a concurrent commit_log test's
        // seal→verify must observe one consistent env for both key reads.
        let _env = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir()
            .join("susi-unwired")
            .join(std::process::id().to_string());
        // SUSI_HOME outranks XDG_CACHE_HOME: drop it so the bus root really
        // moves under a dev-instance launch too.
        std::env::remove_var("SUSI_HOME");
        std::env::set_var("SUSI_XDG", "1");
        std::env::set_var("XDG_CACHE_HOME", &dir);
        // Windows ignores XDG_CACHE_HOME: its cache dir (and so the bus root)
        // is `%LOCALAPPDATA%\intellibitz\susi\cache`. Without moving it every
        // test process shared the user's real bus, and under nextest a
        // sibling process's wired test (`wire_permissive_audit`) answered
        // this process's audit call — the unwired assertions then saw a
        // permitted call and failed.
        if cfg!(windows) {
            std::env::set_var("LOCALAPPDATA", &dir);
        }
    }

    /// Serializes wired vs. unwired audit-gate tests inside this process:
    /// `PlaneBus::global()` is shared, so a `PermitAudit` registration from
    /// a wired test would otherwise stay live and let a later unwired test's
    /// governed call succeed — a false pass. Every test that either wires
    /// the permit or asserts fail-closed holds this lock for its duration.
    pub(super) static AUDIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `PermitAudit` consults this flag — registered once process-wide, it
    /// only answers "permit" while a wired test holds `AUDIT_TEST_LOCK`.
    /// When clear it denies, so `audit_action` fails closed exactly as the
    /// truly-unwired path does.
    pub(super) static AUDIT_PERMIT: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Permissive `gawd.audit.action` handler — only while `AUDIT_PERMIT` is
    /// held by the test that registered it (i.e. holds `AUDIT_TEST_LOCK`).
    /// Shared by every wired-governance test module.
    pub(super) struct PermitAudit;

    impl crate::susi_core::plane_bus::PlaneHandler for PermitAudit {
        fn handle(
            &self,
            _topic: &str,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if AUDIT_PERMIT.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(serde_json::json!({}))
            } else {
                // Registered process-wide on the shared bus — must deny
                // whenever no wired test holds the permit, or an unwired
                // fail-closed assertion running concurrently would pass.
                Err("audit gate closed — no active permissive test".to_string())
            }
        }
    }

    /// Holds `AUDIT_TEST_LOCK` for the test's duration and clears the permit
    /// on drop, so an unwired fail-closed assertion running next still sees
    /// a closed gate.
    pub(super) struct AuditPermitGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for AuditPermitGuard {
        fn drop(&mut self) {
            AUDIT_PERMIT.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Isolate the bus rendezvous, register the permit-aware audit handler,
    /// and open the gate until the returned guard drops.
    pub(super) fn wire_permissive_audit() -> AuditPermitGuard {
        use crate::susi_core::plane_bus::{topics, PlaneBus};
        use std::sync::Arc;
        let guard = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        PlaneBus::global().register(topics::GAWD_AUDIT_ACTION, Arc::new(PermitAudit));
        AUDIT_PERMIT.store(true, std::sync::atomic::Ordering::SeqCst);
        AuditPermitGuard { _lock: guard }
    }

    #[test]
    fn test_reason_fails_closed_when_unwired() {
        let _audit_lock = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        assert!(CoreTools::reason(&serde_json::json!("hi"), Path::new(".")).is_err());
    }

    #[test]
    fn write_file_creates_new_dirs_but_never_escapes() {
        let ws = std::env::temp_dir().join(format!("susi_write_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        let out = CoreTools::write_file(
            &serde_json::json!({"path": "newdir/deeper/a.txt", "content": "hi"}),
            &ws,
        );
        assert!(out.is_ok(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(ws.join("newdir/deeper/a.txt")).unwrap(),
            "hi"
        );
        #[cfg(unix)]
        {
            let outside = ws.with_extension("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, ws.join("dirlink")).unwrap();
            let escaped = CoreTools::write_file(
                &serde_json::json!({"path": "dirlink/pwn.txt", "content": "x"}),
                &ws,
            );
            assert!(escaped.is_err());
            assert!(!outside.join("pwn.txt").exists());
            let _ = std::fs::remove_dir_all(&outside);
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a2a_delegate_never_sends_the_token_to_other_loopback_ports() {
        // Any other local listener (e.g. another user's process) would
        // receive the host bearer token.
        let err = CoreTools::a2a_peer_url("127.0.0.1:18083").unwrap_err();
        assert!(
            err.to_string().contains("limited to the local A2A port"),
            "{err}"
        );
        assert!(CoreTools::a2a_peer_url("http://127.0.0.1:4444/x").is_err());
        assert!(CoreTools::a2a_peer_url("self").is_ok());
    }

    #[test]
    fn test_exec_command_fails_closed_when_unwired() {
        let _audit_lock = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        assert!(CoreTools::exec_command(&serde_json::json!("ls"), Path::new(".")).is_err());
    }

    #[test]
    fn test_os_tools_fail_closed_when_unwired() {
        let _audit_lock = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        assert!(CoreTools::os_ps(&serde_json::json!({}), Path::new(".")).is_err());
        assert!(CoreTools::os_sysinfo(&serde_json::json!({}), Path::new(".")).is_err());
        assert!(CoreTools::os_services(&serde_json::json!({}), Path::new(".")).is_err());
        assert!(CoreTools::os_kill(&serde_json::json!({"pid": 1}), Path::new(".")).is_err());
        assert!(CoreTools::commit_record(&serde_json::json!({}), Path::new(".")).is_err());
    }
}

#[cfg(test)]
mod os_tools_wired_tests {
    //! Positive-path coverage: a permissive `gawd.audit.action` handler is
    //! registered on a pid-private bus root, so the governed tools actually
    //! execute instead of failing closed at the audit gate.
    use super::unwired_governance_tests::wire_permissive_audit;
    use crate::tools::CoreTools;
    use std::path::Path;
    use std::path::PathBuf;

    #[test]
    fn os_services_lists_all_five_leaf_services() {
        let _permit = wire_permissive_audit();
        let out = CoreTools::os_services(&serde_json::json!({}), Path::new("."))
            .expect("os_services status");
        let statuses: serde_json::Value = serde_json::from_str(&out).expect("json array");
        assert_eq!(statuses.as_array().map(|a| a.len()), Some(5));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn os_ps_lists_this_test_process() {
        let _permit = wire_permissive_audit();
        let out =
            CoreTools::os_ps(&serde_json::json!({"limit": 500}), Path::new(".")).expect("os_ps");
        assert!(out.contains("PID\tNAME\tRSS_KB"));
        assert!(out.contains(&std::process::id().to_string()));
    }

    #[test]
    fn os_logs_rejects_unknown_service_and_missing_log() {
        let _permit = wire_permissive_audit();
        let err = CoreTools::os_logs(&serde_json::json!({"name": "not-a-svc"}), Path::new("."));
        assert!(err.is_err(), "unknown service must fail");

        // A real leaf name with no supervised log must error cleanly.
        let err = CoreTools::os_logs(&serde_json::json!({"name": "susi-paths"}), Path::new("."));
        // Either a missing log error or — if the host actually has a log —
        // content; never a panic or wrong-service read.
        if let Err(e) = err {
            assert!(e.to_string().contains("no log"), "got: {e}");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn os_sysinfo_reports_kernel_and_memory() {
        let _permit = wire_permissive_audit();
        let out =
            CoreTools::os_sysinfo(&serde_json::json!({}), Path::new(".")).expect("os_sysinfo");
        assert!(out.contains("kernel: Linux"));
        assert!(out.contains("MemTotal:"));
    }

    #[test]
    fn os_kill_fails_closed_on_unsupervised_pid() {
        let _permit = wire_permissive_audit();
        // Audit passes, but the pid gate must still refuse: pid 4_000_000
        // is never in the substrate process table.
        let err = CoreTools::os_kill(
            &serde_json::json!({"pid": 4_000_000, "signal": "term"}),
            Path::new("."),
        )
        .expect_err("unsupervised pid must be refused");
        assert!(err.to_string().contains("supervised"));
    }

    #[test]
    fn os_kill_fails_closed_on_external_table_row() {
        let _permit = wire_permissive_audit();
        // External rows record processes the daemon observed but did not
        // spawn — the kill gate must exclude them. Write one in, verify
        // refusal, restore the table (the supervisor would also drop the
        // stale row on its next pass: the fake port is dead).
        let path = crate::susi_core::service_table::table_path();
        let original = std::fs::read_to_string(&path).unwrap_or_default();
        let mut table = crate::susi_core::service_table::load();
        crate::susi_core::service_table::record_external(&mut table, "susi-native", 4_000_001, 1);
        crate::susi_core::service_table::save(&table).expect("seed external row");
        let err = CoreTools::os_kill(
            &serde_json::json!({"pid": 4_000_001, "signal": "term"}),
            Path::new("."),
        )
        .expect_err("external row must not be killable");
        assert!(err.to_string().contains("supervised"));
        if original.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::write(&path, original).expect("restore table");
        }
    }

    #[test]
    fn os_kill_rejects_non_whitelisted_signals() {
        let _permit = wire_permissive_audit();
        assert!(CoreTools::os_kill(
            &serde_json::json!({"pid": 1, "signal": "STOP"}),
            Path::new("."),
        )
        .is_err());
    }

    /// Isolated config dir so `cluster.key` and `commit_log.jsonl` are
    /// created/read under a temp root, never the real `~/.susi`. Holds the
    /// shared env lock for the returned guard's lifetime — `cluster_key()`
    /// resolves through XDG env vars and other tests seal/verify against
    /// it, so the mutation must be exclusive for the whole test.
    pub(super) fn isolate_config() -> (
        std::sync::MutexGuard<'static, ()>,
        susi_paths::test_env::EnvGuard,
        PathBuf,
    ) {
        let guard = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir()
            .join("susi-commit-test")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp config dir");
        // Also scrubs SUSI_HOME/SUSI_PORT_OFFSET, which outrank XDG_CONFIG_HOME.
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("XDG_CONFIG_HOME", &dir);
        // susi_paths only honors XDG vars under SUSI_XDG when a legacy
        // ~/.susi exists — without this the tests would write the real
        // host's term.json/commit_log.jsonl.
        env.set("SUSI_XDG", "1");
        // Windows resolves the config root through APPDATA, not XDG: without
        // these the test reads the real host's commit_log.jsonl.
        env.set("APPDATA", dir.join("appdata"));
        env.set("LOCALAPPDATA", dir.join("localappdata"));
        (guard, env, dir)
    }

    #[test]
    fn commit_record_accepts_signed_record_and_appends_to_ledger() {
        let _permit = wire_permissive_audit();
        let (_lock, _env, dir) = isolate_config();
        let Some(rec) = crate::susi_core::commit_log::CommitRecord::seal(
            crate::susi_core::commit_log::CommitInput {
                coordinator: "susi-local-master",
                leader: "susi-local-master",
                electorate: vec!["AgentA".into(), "AgentB".into(), "PeerNode_1".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "committed answer",
            },
        ) else {
            eprintln!("skip: cluster key unavailable");
            return;
        };
        let out = CoreTools::commit_record(
            &serde_json::to_value(&rec).expect("record to json"),
            Path::new("."),
        )
        .expect("signed record must be accepted");
        assert!(out.contains("accepted"));

        let ledger = crate::susi_core::commit_log::load();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].value, "committed answer");

        let list =
            CoreTools::commit_log(&serde_json::json!({}), Path::new(".")).expect("commit_log list");
        assert!(list.contains("susi-local-master"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_record_rejects_forged_and_unsigned_records() {
        let _permit = wire_permissive_audit();
        let (_lock, _env, dir) = isolate_config();
        // Forged: no valid signature.
        let forged = serde_json::json!({
            "epoch": "aa", "coordinator": "evil-peer", "electorate": ["A", "B"],
            "tally": 2, "quorum_threshold": 2, "value_hash": "h",
            "value": "v", "committed_at": 0, "signature": "deadbeef"
        });
        assert!(CoreTools::commit_record(&forged, Path::new(".")).is_err());
        // And a correctly-shaped record whose value was tampered post-signing.
        if let Some(rec) = crate::susi_core::commit_log::CommitRecord::seal(
            crate::susi_core::commit_log::CommitInput {
                coordinator: "susi-local-master",
                leader: "susi-local-master",
                electorate: vec!["A".into(), "B".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "honest",
            },
        ) {
            let mut tampered = serde_json::to_value(&rec).expect("json");
            tampered["value"] = serde_json::json!("forged payload");
            assert!(CoreTools::commit_record(&tampered, Path::new(".")).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_records_batch_intake_applies_skips_refuses() {
        let _permit = wire_permissive_audit();
        let (_lock, _env, dir) = isolate_config();
        let seal = |value: &str| {
            crate::susi_core::commit_log::CommitRecord::seal(
                crate::susi_core::commit_log::CommitInput {
                    coordinator: "susi-local-master",
                    leader: "susi-local-master",
                    electorate: vec!["A".into(), "B".into()],
                    tally: 2,
                    quorum_threshold: 2,
                    value,
                },
            )
        };
        let (Some(r1), Some(r2)) = (seal("v1"), seal("v2")) else {
            eprintln!("skip: cluster key unavailable");
            return;
        };
        // Seal produces seq from the ledger at seal time — r1 and r2 may
        // share a seq when the ledger is empty; the batch still applies
        // both (dup-seq handling lives in append_many).
        let out =
            CoreTools::commit_records(&serde_json::json!({ "records": [r1, r2] }), Path::new("."))
                .expect("batch must be accepted");
        let counts: serde_json::Value = serde_json::from_str(&out).expect("json summary");
        assert!(counts["applied"].as_u64().unwrap_or(0) >= 1);

        // Replay the same batch — everything held → all skipped, none applied.
        let out = CoreTools::commit_records(
            &serde_json::json!({ "records": [
                crate::susi_core::commit_log::load()[0].clone()
            ] }),
            Path::new("."),
        )
        .expect("re-batch must be accepted");
        let counts: serde_json::Value = serde_json::from_str(&out).expect("json summary");
        assert_eq!(counts["applied"].as_u64(), Some(0));

        // A forged record inside an otherwise fine batch is refused, not fatal.
        let forged = serde_json::json!({
            "epoch": "aa", "coordinator": "evil", "electorate": ["A", "B"],
            "tally": 2, "quorum_threshold": 2, "value_hash": "h",
            "value": "v", "committed_at": 0, "signature": "deadbeef",
            "seq": 1, "term": 1, "leader": "evil", "prev_epoch": "",
            "member_sig": "", "member_pubkey": "", "subject_sig": "",
            "endorsements": []
        });
        let out =
            CoreTools::commit_records(&serde_json::json!({ "records": [forged] }), Path::new("."))
                .expect("mixed batch still returns a summary");
        let counts: serde_json::Value = serde_json::from_str(&out).expect("json summary");
        assert_eq!(counts["refused"].as_u64(), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_log_fetch_filters_by_coordinator_and_seq() {
        let _permit = wire_permissive_audit();
        let (_lock, _env, dir) = isolate_config();
        let seal = |value: &str| {
            crate::susi_core::commit_log::CommitRecord::seal(
                crate::susi_core::commit_log::CommitInput {
                    coordinator: "c",
                    leader: "c",
                    electorate: vec!["A".into(), "B".into()],
                    tally: 2,
                    quorum_threshold: 2,
                    value,
                },
            )
        };
        let (Some(r1), ..) = (seal("v1"),) else {
            return;
        };
        crate::susi_core::commit_log::append(&r1).expect("append r1");
        // seq is assigned from the ledger at seal time — append first so
        // the second record gets seq 2.
        let Some(r2) = seal("v2") else { return };
        assert_eq!(r2.seq, 2);
        crate::susi_core::commit_log::append(&r2).expect("append r2");

        let out = CoreTools::commit_log_fetch(
            &serde_json::json!({"coordinator": "c", "from_seq": 2}),
            Path::new("."),
        )
        .expect("fetch");
        let got: Vec<crate::susi_core::commit_log::CommitRecord> =
            serde_json::from_str(&out).expect("records json");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].seq, 2);
        assert_eq!(got[0].value, "v2");

        // limit bounds the pull even when more records match.
        let out = CoreTools::commit_log_fetch(
            &serde_json::json!({"coordinator": "c", "limit": 1}),
            Path::new("."),
        )
        .expect("fetch limited");
        let got: Vec<crate::susi_core::commit_log::CommitRecord> =
            serde_json::from_str(&out).expect("records json");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].value, "v1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_record_reports_gap_when_repair_unreachable() {
        let _permit = wire_permissive_audit();
        let (_lock, _env, dir) = isolate_config();
        let seal = |value: &str| {
            crate::susi_core::commit_log::CommitRecord::seal(
                crate::susi_core::commit_log::CommitInput {
                    coordinator: "ghost-coordinator",
                    leader: "ghost-coordinator",
                    electorate: vec!["A".into(), "B".into()],
                    tally: 2,
                    quorum_threshold: 2,
                    value,
                },
            )
        };
        let (Some(r3),) = (seal("v3"),) else { return };
        // seq 3 arrives with nothing held for this coordinator → gap
        // [1,2]; repair can't resolve "ghost-coordinator" in the roster
        // (no gawd.cluster.peers handler on the test bus), so the
        // response must surface the warning, not swallow it.
        let mut r3 = r3;
        r3.seq = 3;
        if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
            r3.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &k,
                r3.signed_payload().as_bytes(),
            );
        }
        let out =
            CoreTools::commit_record(&serde_json::to_value(&r3).expect("json"), Path::new("."))
                .expect("gap record still commits");
        assert!(out.contains("WARNING: replication gap"), "got: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_record_rejects_stale_term_and_adopts_newer() {
        let _permit = wire_permissive_audit();
        let (_lock, _env, dir) = isolate_config();
        // Coordinator authority requires explicit membership — declare
        // the test's coordinators in the isolated roster so their
        // records may drive term state.
        let peers_dir = susi_paths::SusiDirs::config_dir();
        std::fs::create_dir_all(&peers_dir).expect("peers dir");
        std::fs::write(
            peers_dir.join("peers.json"),
            serde_json::to_string(&serde_json::json!([
                { "node_id": "leader-a", "admission": "explicit" },
                { "node_id": "leader-c", "admission": "explicit" },
            ]))
            .expect("peers json"),
        )
        .expect("write peers.json");
        // Establish term 1 under leader-a, seal a record there, then move
        // the cluster to term 2 — the term-1 push must now be rejected.
        crate::susi_core::commit_log::claim_leadership("leader-a");
        let Some(rec) = crate::susi_core::commit_log::CommitRecord::seal(
            crate::susi_core::commit_log::CommitInput {
                coordinator: "leader-a",
                leader: "leader-a",
                electorate: vec!["A".into(), "B".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "term-1 decision",
            },
        ) else {
            eprintln!("skip: cluster key unavailable");
            return;
        };
        assert_eq!(rec.term, 1);
        crate::susi_core::commit_log::claim_leadership("leader-b");
        assert_eq!(crate::susi_core::commit_log::load_term().term, 2);

        let err =
            CoreTools::commit_record(&serde_json::to_value(&rec).expect("json"), Path::new("."));
        assert!(err.is_err(), "stale-term push must be rejected");
        assert!(err.unwrap_err().to_string().contains("stale term"));

        // A record from a newer term is accepted AND adopts the term.
        let mut ahead = rec.clone();
        ahead.term = 7;
        ahead.leader = "leader-c".into();
        if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
            ahead.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &k,
                ahead.signed_payload().as_bytes(),
            );
        }
        let out =
            CoreTools::commit_record(&serde_json::to_value(&ahead).expect("json"), Path::new("."))
                .expect("newer-term record accepted");
        assert!(out.contains("adopted term 7"), "got: {out}");
        assert_eq!(crate::susi_core::commit_log::load_term().term, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod core_io_tests {
    use super::unwired_governance_tests::wire_permissive_audit;
    use crate::tools::CoreTools;
    use serde_json::json;
    use std::path::PathBuf;

    fn ws() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-coretools-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_then_read_file_round_trips_inside_the_workspace() {
        let ws = ws();
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        let out =
            CoreTools::write_file(&json!({"path": "sub/f.txt", "content": "hello susi"}), &ws)
                .unwrap();
        assert!(out.contains("Wrote to"));
        assert_eq!(
            CoreTools::read_file(&json!({"path": "sub/f.txt"}), &ws).unwrap(),
            "hello susi"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn file_tools_reject_escapes_and_missing_args() {
        let ws = ws();
        for bad in [
            json!({"path": "/etc/passwd", "content": "x"}),
            json!({"path": "../x", "content": "x"}),
            json!({"path": "x"}), // content missing
            json!({}),            // both missing
        ] {
            assert!(CoreTools::write_file(&bad, &ws).is_err(), "{bad}");
        }
        for bad in [
            json!({"path": "/etc/passwd"}),
            json!({"path": "../x"}),
            json!({}),
        ] {
            assert!(CoreTools::read_file(&bad, &ws).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_file_missing_leaf_reports_typed_error() {
        let ws = ws();
        // The leaf does not exist; resolution succeeds, the open fails typed.
        let res = CoreTools::read_file(&json!({"path": "nope.txt"}), &ws);
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn exec_command_runs_confined_commands() {
        let _permit = wire_permissive_audit();
        let ws = ws();
        let out = CoreTools::exec_command(&json!("rustc --version"), &ws).unwrap();
        assert!(out.contains("rustc "), "got: {out}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn exec_command_rejects_empty_escape_and_bad_syntax() {
        let _permit = wire_permissive_audit();
        let ws = ws();
        assert!(CoreTools::exec_command(&json!(""), &ws).is_err());
        assert!(CoreTools::exec_command(&json!("echo ../out"), &ws).is_err());
        assert!(CoreTools::exec_command(&json!("cat /etc/passwd"), &ws).is_err());
        assert!(CoreTools::exec_command(&json!("echo 'unterminated"), &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn exec_command_surfaces_nonzero_exit() {
        let _permit = wire_permissive_audit();
        let ws = ws();
        let res = CoreTools::exec_command(&json!("rustc --susi-invalid-fixture-option"), &ws);
        let error = res.expect_err("an actual native compiler failure must propagate");
        assert!(
            error.to_string().contains("susi-invalid-fixture-option"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn status_and_os_probes_return_real_strings() {
        let ws = ws();
        let s = CoreTools::status(&json!({}), &ws).unwrap();
        assert!(s.contains("SUSI Engine Version"));
        #[cfg(target_os = "linux")]
        {
            let _permit = wire_permissive_audit();
            let ps = CoreTools::os_ps(&json!({"limit": 3}), &ws).unwrap();
            assert!(ps.starts_with("PID\tNAME"));
            let info = CoreTools::os_sysinfo(&json!({}), &ws).unwrap();
            assert!(info.contains("kernel:"));
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn swarm_schedule_reports_empty_when_no_decisions() {
        let ws = ws();
        // No scheduler handler on the bus → the "no missions" early return.
        let out = CoreTools::swarm_schedule(&json!({}), &ws).unwrap();
        assert!(out.contains("No missions") || !out.is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn agents_tools_validate_required_fields() {
        let ws = ws();
        assert!(CoreTools::agents_run(&json!({}), &ws).is_err());
        assert!(CoreTools::agents_run(&json!({"agent": "aider"}), &ws).is_err());
        assert!(CoreTools::agents_status(&json!({}), &ws).is_err());
        // List/tasks survive without external agent state.
        assert!(CoreTools::agents_list(&json!({}), &ws).is_ok());
        assert!(CoreTools::agents_tasks(&json!({}), &ws).is_ok());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn extensions_tools_admit_commercial_default_pack_over_mcp_wire() {
        use crate::mcp::ServiceExt;
        use crate::protocol::GmcpService;
        use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
        use sha2::{Digest, Sha256};
        use std::sync::Arc;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        struct ExtensionRouter(PathBuf);
        impl PlaneHandler for ExtensionRouter {
            fn handle(
                &self,
                topic: &str,
                payload: serde_json::Value,
            ) -> Result<serde_json::Value, String> {
                match topic {
                    topics::TOOLS_LIST => Ok(json!([
                        {"name": "extensions_list", "description": "list"},
                        {"name": "extensions_status", "description": "status"},
                        {"name": "extensions_validate", "description": "validate"},
                        {"name": "extensions_load", "description": "load"},
                        {"name": "extensions_unload", "description": "unload"},
                        {"name": "agents_list", "description": "agents"},
                        {"name": "frameworks_list", "description": "frameworks"},
                    ])),
                    topics::TOOLS_EXISTS => Ok(json!({"exists": true})),
                    topics::TOOLS_EXECUTE => {
                        let name = payload["name"].as_str().unwrap_or("");
                        let args = &payload["args"];
                        let out = match name {
                            "extensions_list" => CoreTools::extensions_list(args, &self.0),
                            "extensions_status" => CoreTools::extensions_status(args, &self.0),
                            "extensions_validate" => CoreTools::extensions_validate(args, &self.0),
                            "extensions_load" => CoreTools::extensions_load(args, &self.0),
                            "extensions_unload" => CoreTools::extensions_unload(args, &self.0),
                            "agents_list" => CoreTools::agents_list(args, &self.0),
                            "frameworks_list" => CoreTools::frameworks_list(args, &self.0),
                            other => return Err(format!("unexpected tool {other}")),
                        }
                        .map_err(|e| e.to_string())?;
                        Ok(json!({"text": out}))
                    }
                    other => Err(format!("unexpected topic {other}")),
                }
            }
        }

        let (_lock, mut env, dir) = super::os_tools_wired_tests::isolate_config();
        env.set("SUSI_HOME", &dir);
        env.set("SUSI_XDG", "0");
        env.remove("SUSI_EXTENSION_PACK");
        crate::susi_config::extensions::invalidate_extension_caches();

        let ws = dir.join("workspace");
        std::fs::create_dir_all(&ws).unwrap();

        // Install a full `commercial-default` pack under `<config_dir>/extensions/commercial-default`
        // matching `susi-commercial-apps/crates/susi-ecosystem-hub/packs/commercial-default`.
        let pack_root = susi_paths::SusiDirs::config_dir()
            .join("extensions")
            .join("commercial-default");
        std::fs::create_dir_all(&pack_root).unwrap();

        let files: &[(&str, &str)] = &[
            (
                "leading-mcp.json",
                r#"[{"id":"filesystem","name":"Filesystem MCP","rank":1,"documentation":"https://modelcontextprotocol.io","package":"@modelcontextprotocol/server-filesystem","runner":"npx","args":["-y","@modelcontextprotocol/server-filesystem","{workspace}"],"env_keys":[],"category":"fs","notes":"Bedrock server"}]"#,
            ),
            (
                "mcp-registry.json",
                r#"[{"id":"filesystem","name":"Filesystem MCP","package":"@modelcontextprotocol/server-filesystem"}]"#,
            ),
            (
                "execution-agents.json",
                r#"[{"id":"codex","name":"Codex Agent","peer_name":"CodexAgent","rank":1,"documentation":"https://developers.openai.com/codex/cli","adapter":{"kind":"command","program":"codex","args":["exec","--","{prompt}"]}}]"#,
            ),
            (
                "agent-engines.json",
                r#"[{"id":"langgraph","name":"LangGraph","peer_name":"LangGraphEngine","rank":1,"documentation":"https://langchain-ai.github.io/langgraph/","adapter":{"kind":"command","program":"langgraph","args":["run","--","{prompt}"]}}]"#,
            ),
            ("external-peer-agents.json", "[]"),
            ("bootstrap-mcp-servers.json", "[]"),
        ];

        let mut file_map = serde_json::Map::new();
        let mut checksums = serde_json::Map::new();
        for (name, body) in files {
            std::fs::write(pack_root.join(name), body.as_bytes()).unwrap();
            let hex = susi_paths::hex::encode(Sha256::digest(body.as_bytes()));
            file_map.insert((*name).to_string(), json!(name));
            checksums.insert((*name).to_string(), json!(format!("sha256:{hex}")));
        }

        let manifest = json!({
            "api_version": "1",
            "id": "commercial-default",
            "name": "Susi Commercial Default Vendor Catalogs",
            "version": "1.1.0",
            "description": "Launchable third-party vendor catalogs for Susi",
            "publisher": "IntelliBitz Susi Commercial",
            "capabilities": [
                "catalog.mcp",
                "catalog.mcp_registry",
                "catalog.execution_agents",
                "catalog.agent_engines"
            ],
            "permissions": [],
            "requires": [],
            "optional": [],
            "files": file_map,
            "checksums": checksums,
            "signatures": {}
        });
        std::fs::write(
            pack_root.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Held to the end of the test: `GmcpService` resolves `tools/list` and
        // `tools/call` through these process-global topics, which other tests
        // replace with their own stubs.
        let _tools_plane = crate::protocol_tests::TOOLS_PLANE_TEST_LOCK.blocking_lock();
        let router = Arc::new(ExtensionRouter(ws.clone()));
        PlaneBus::global().register(topics::TOOLS_LIST, router.clone());
        PlaneBus::global().register(topics::TOOLS_EXISTS, router.clone());
        PlaneBus::global().register(topics::TOOLS_EXECUTE, router);

        let rt = CoreTools::shared_runtime().unwrap();
        rt.block_on(async {
            let (client, server) = tokio::io::duplex(1 << 20);
            let service_ws = ws.clone();
            tokio::spawn(async move {
                let running = GmcpService::new(service_ws).serve(server).await.unwrap();
                let _ = running.waiting().await;
            });
            let mut wire = BufReader::new(client);
            let mut rpc =
                async |id: i64, method: &str, params: serde_json::Value| -> serde_json::Value {
                    let msg =
                        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
                    wire.get_mut()
                        .write_all(format!("{msg}\n").as_bytes())
                        .await
                        .unwrap();
                    let mut line = String::new();
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        wire.read_line(&mut line),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    serde_json::from_str(&line).unwrap()
                };

            let init = rpc(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "commercial-pack-test", "version": "1.0"}
                }),
            )
            .await;
            assert_eq!(init["result"]["protocolVersion"], "2025-11-25");

            // 1. Validate `commercial-default` over the MCP socket.
            let validated = rpc(
                2,
                "tools/call",
                json!({
                    "name": "extensions_validate",
                    "arguments": { "path": pack_root.to_string_lossy() }
                }),
            )
            .await;
            assert_ne!(validated["result"]["isError"], json!(true), "{validated}");
            let validated_json: serde_json::Value =
                serde_json::from_str(validated["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(validated_json["valid"], json!(true));
            assert_eq!(validated_json["id"], json!("commercial-default"));
            assert_eq!(validated_json["version"], json!("1.1.0"));
            assert!(validated_json["artifacts_sha256"]["manifest.json"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert!(validated_json["artifacts_sha256"]["leading-mcp.json"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));

            // 2. Load `commercial-default` over the MCP socket.
            let loaded = rpc(
                3,
                "tools/call",
                json!({
                    "name": "extensions_load",
                    "arguments": { "id": "commercial-default" }
                }),
            )
            .await;
            assert_ne!(loaded["result"]["isError"], json!(true), "{loaded}");
            let loaded_json: serde_json::Value =
                serde_json::from_str(loaded["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(loaded_json["active"], json!("commercial-default"));
            assert_eq!(loaded_json["loaded"]["id"], json!("commercial-default"));
            assert_eq!(loaded_json["loaded"]["loaded"], json!(true));
            assert_eq!(loaded_json["loaded"]["active"], json!(true));
            assert_eq!(loaded_json["loaded"]["quarantined"], json!(false));

            // 3. Verify `extensions_status` and `extensions_list` over the MCP socket.
            let status = rpc(
                4,
                "tools/call",
                json!({
                    "name": "extensions_status",
                    "arguments": {}
                }),
            )
            .await;
            let status_json: serde_json::Value =
                serde_json::from_str(status["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(status_json["active"], json!("commercial-default"));

            let listed = rpc(
                5,
                "tools/call",
                json!({
                    "name": "extensions_list",
                    "arguments": {}
                }),
            )
            .await;
            let listed_json: serde_json::Value =
                serde_json::from_str(listed["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert!(listed_json
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == "commercial-default"
                    && p["active"] == true
                    && p["loaded"] == true));

            // 4. Mandate 62 proof: the admitted pack's catalogs are immediately served over the
            //    socket without rebuilding Susi.
            let loaded_mcp: Vec<serde_json::Value> =
                crate::susi_config::extensions::load_json_or_bundled("leading-mcp.json", "[]");
            assert_eq!(loaded_mcp.len(), 1);
            assert_eq!(loaded_mcp[0]["id"], json!("filesystem"));

            // 5. Unload `commercial-default` over the MCP socket and verify fallback to `default`.
            let unloaded = rpc(
                6,
                "tools/call",
                json!({
                    "name": "extensions_unload",
                    "arguments": { "id": "commercial-default" }
                }),
            )
            .await;
            assert_ne!(unloaded["result"]["isError"], json!(true), "{unloaded}");
            let unloaded_json: serde_json::Value =
                serde_json::from_str(unloaded["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(unloaded_json["unloaded"], json!("commercial-default"));
            assert_eq!(unloaded_json["active"], json!("default"));

            let after_unload_mcp: Vec<serde_json::Value> =
                crate::susi_config::extensions::load_json_or_bundled("leading-mcp.json", "[]");
            assert!(after_unload_mcp.is_empty());
        });

        crate::susi_config::extensions::invalidate_extension_caches();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod task_control_tests {
    use crate::susi_core::task_manager::{SwarmTaskManager, TaskStatus};
    use crate::susi_error::EaiResult;
    use crate::tools::CoreTools;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::atomic::Ordering;

    type Control = fn(&Value, &Path) -> EaiResult<String>;

    // Remove only this test's record and flags, including after an assertion panic.
    struct OwnedTask(String);
    impl Drop for OwnedTask {
        fn drop(&mut self) {
            let manager = SwarmTaskManager::global();
            manager
                .tasks
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
            manager
                .cancel_map
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
            manager
                .pause_map
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
        }
    }

    #[test]
    fn task_controls_preserve_arguments_messages_and_owned_task_transitions() {
        crate::test_isolation::with_isolated_home(|workspace| {
            let controls: [(Control, &str, TaskStatus); 3] = [
                (CoreTools::tasks_pause, "paused", TaskStatus::Paused),
                (CoreTools::tasks_resume, "resumed", TaskStatus::Running),
                (CoreTools::tasks_kill, "killed", TaskStatus::Killed),
            ];
            let scope = format!("gmcp-task-control-{}", workspace.display());
            for object in [false, true] {
                let task = SwarmTaskManager::global().register_task_scoped(
                    &scope,
                    "task control regression",
                    Some(&scope),
                );
                let _owned = OwnedTask(task.task_id.clone());
                assert_eq!(
                    task.status.load(Ordering::Acquire),
                    TaskStatus::Running as u8
                );

                for (control, _, _) in controls {
                    for arg in [
                        json!({}),
                        Value::Null,
                        json!({"task_id": null}),
                        json!({"task_id": false}),
                        json!({"task_id": 7}),
                        json!({"task_id": []}),
                        json!({"task_id": {}}),
                        json!({"task_id": " \t\n"}),
                        json!(" \t\n"),
                    ] {
                        let error = control(&arg, workspace).unwrap_err();
                        assert_eq!(error.code(), "susi.protocol");
                        assert_eq!(error.to_string(), "Protocol Error: Task '' not found.");
                    }
                }
                assert_eq!(
                    task.status.load(Ordering::Acquire),
                    TaskStatus::Running as u8
                );
                assert!(!task.pause_flag.load(Ordering::Acquire));
                assert!(!task.cancel_flag.load(Ordering::Acquire));

                let padded = format!(" \t{}\n", task.task_id);
                let arg = if object {
                    json!({"task_id": padded, "extra": true})
                } else {
                    json!(padded)
                };
                for (control, verb, status) in controls {
                    assert_eq!(
                        control(&arg, workspace).unwrap(),
                        format!("Task '{}' {verb}.", task.task_id)
                    );
                    assert_eq!(task.status.load(Ordering::Acquire), status as u8);
                    assert_eq!(
                        task.pause_flag.load(Ordering::Acquire),
                        status == TaskStatus::Paused
                    );
                    assert_eq!(
                        task.cancel_flag.load(Ordering::Acquire),
                        status == TaskStatus::Killed
                    );
                }
                for (control, _, _) in controls {
                    assert_eq!(
                        control(&arg, workspace).unwrap_err().to_string(),
                        format!("Protocol Error: Task '{}' not found.", task.task_id),
                    );
                }
                assert_eq!(
                    task.status.load(Ordering::Acquire),
                    TaskStatus::Killed as u8
                );
                assert_eq!(
                    task.result.read().unwrap().as_deref(),
                    Some("Killed by pulse")
                );
            }
        });
    }
}
