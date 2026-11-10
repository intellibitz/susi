use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

fn test_def(id: &str, peer: &str, rank: u8) -> AgentDefinition {
    AgentDefinition {
        id: id.into(),
        name: id.into(),
        peer_name: peer.into(),
        rank,
        documentation: "https://example.invalid".into(),
        adapter: Adapter::A2a {
            url: "http://127.0.0.1:18080/a2a".into(),
            token_env: None,
        },
        extra: std::collections::HashMap::new(),
    }
}

fn local_adapter() -> Adapter {
    let exe = std::env::current_exe().expect("test binary path");
    Adapter::Command {
        program: exe.to_string_lossy().into_owned(),
        args: vec!["--help".into()],
    }
}

fn temp_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("susi-ext-mod-{tag}-{}", unique_id().unwrap()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn run_status_active_distinguishes_in_flight_from_terminal_states() {
    assert!(RunStatus::Queued.active());
    assert!(RunStatus::Running.active());
    assert!(!RunStatus::Waiting.active());
    assert!(!RunStatus::Succeeded.active());
    assert!(!RunStatus::Failed.active());
    assert!(!RunStatus::Cancelled.active());
    assert!(!RunStatus::Stopped.active());
    assert!(!RunStatus::Unknown.active());
}

#[test]
fn manager_with_config_validates_workspace_and_accessors() {
    let root = temp_workspace("cfg");
    let missing = root.join("does-not-exist");
    let err = AgentManager::with_config(&missing, CatalogKind::Execution, root.join("cfg"))
        .err()
        .expect("missing workspace must fail");
    assert!(
        err.to_string().contains("workspace does not exist"),
        "{err}"
    );

    let file_ws = root.join("not-a-dir.txt");
    fs::write(&file_ws, b"file").unwrap();
    let err = AgentManager::with_config(&file_ws, CatalogKind::Execution, root.join("cfg"))
        .err()
        .expect("file workspace must fail");
    assert!(
        err.to_string()
            .contains("agent workspace must be a directory"),
        "{err}"
    );

    let exec_mgr = AgentManager::new(&root)
        .unwrap()
        .with_poll_interval(Duration::from_millis(125));
    assert_eq!(exec_mgr.kind(), CatalogKind::Execution);
    assert_eq!(exec_mgr.poll_interval(), Duration::from_millis(125));

    let fw_mgr = AgentManager::frameworks(&root).unwrap();
    assert_eq!(fw_mgr.kind(), CatalogKind::Framework);
    assert_eq!(fw_mgr.poll_interval(), Duration::from_secs(3));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn prepare_adapter_enforces_prompt_rules_and_self_build_wrapping() {
    let root = temp_workspace("prep");
    fs::create_dir_all(root.join(".agents")).unwrap();
    fs::write(root.join(".agents/identity.json"), "{}").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"susi\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let exec_mgr = AgentManager::with_config(&root, CatalogKind::Execution, root.join("cfg"))
        .unwrap()
        .with_catalog(vec![
            test_def("codex", "CodexAgent", 1),
            test_def("browser-use", "BrowserUseAgent", 2),
        ]);

    let a2a = Adapter::A2a {
        url: "http://127.0.0.1:18080/a2a".into(),
        token_env: None,
    };

    let empty_err = exec_mgr
        .prepare_adapter("codex", "   \n\t  ", a2a.clone())
        .unwrap_err();
    assert!(
        empty_err
            .to_string()
            .contains("task prompt must not be empty"),
        "{empty_err}"
    );

    let coding_run = exec_mgr
        .prepare_adapter("codex", "implement feature X", a2a.clone())
        .unwrap();
    assert_eq!(coding_run.status, RunStatus::Queued);
    assert!(
        coding_run
            .prompt
            .starts_with(crate::susi_core::self_build::BRIEF),
        "coding executor in susi repo must prepend self-build contract"
    );
    assert!(coding_run.prompt.ends_with("implement feature X"));

    let non_coding_run = exec_mgr
        .prepare_adapter("browser-use", "search docs", a2a.clone())
        .unwrap();
    assert_eq!(non_coding_run.prompt, "search docs");

    let fw_mgr = AgentManager::with_config(&root, CatalogKind::Framework, root.join("fw-cfg"))
        .unwrap()
        .with_catalog(vec![test_def("langgraph", "LangGraphEngine", 1)]);
    let fw_run = fw_mgr
        .prepare_adapter("langgraph", "orchestrate graph", a2a)
        .unwrap();
    assert_eq!(fw_run.prompt, "orchestrate graph");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn worker_lease_rejects_competing_mutations_until_released() {
    let root = temp_workspace("lease-owner");
    let mgr = AgentManager::with_config(&root, CatalogKind::Execution, root.join("cfg"))
        .unwrap()
        .with_catalog(vec![test_def("codex", "CodexAgent", 1)]);
    let run = mgr
        .prepare_adapter("codex", "local fixture", local_adapter())
        .unwrap();
    let lease = mgr.acquire(&run.id).unwrap();
    for error in [
        mgr.execute(&run.id).unwrap_err(),
        mgr.refresh(&run.id).unwrap_err(),
        mgr.send(&run.id, "continue").unwrap_err(),
        mgr.mark_launch_failed(&run.id, "must not mutate")
            .unwrap_err(),
    ] {
        assert!(
            error
                .to_string()
                .contains("task is owned by a running worker"),
            "{error}"
        );
    }
    assert_eq!(mgr.read(&run.id).unwrap().status, RunStatus::Queued);
    drop(lease);
    mgr.mark_launch_failed(&run.id, "after release").unwrap();
    assert_eq!(mgr.read(&run.id).unwrap().status, RunStatus::Failed);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn worker_lease_unlocks_with_an_inherited_open_file_description() {
    let root = temp_workspace("lease-inherited");
    let mgr = AgentManager::with_config(&root, CatalogKind::Execution, root.join("cfg"))
        .unwrap()
        .with_catalog(vec![test_def("codex", "CodexAgent", 1)]);
    let run = mgr
        .prepare_adapter("codex", "local fixture", local_adapter())
        .unwrap();
    let lease = mgr.acquire(&run.id).unwrap();
    // A duplicate shares the same open file description, exactly as a file
    // inherited across fork. Keep it open to make the release race deterministic
    // without spawning a worker or relying on scheduler timing.
    let inherited = lease.file.try_clone().unwrap();
    assert!(mgr.acquire(&run.id).is_err());
    drop(lease);
    let next = mgr.acquire(&run.id).unwrap();
    // Closing the stale inherited descriptor must not unlock the new owner.
    drop(inherited);
    assert!(mgr.acquire(&run.id).is_err());
    drop(next);
    let final_lease = mgr.acquire(&run.id).unwrap();
    drop(final_lease);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mark_launch_failed_refresh_send_logs_and_shutdown_cancellation_work_end_to_end() {
    let root = temp_workspace("lifecycle");
    let shutdown = Arc::new(AtomicBool::new(false));
    let mgr = AgentManager::with_config(&root, CatalogKind::Execution, root.join("cfg"))
        .unwrap()
        .with_catalog(vec![test_def("codex", "CodexAgent", 1)])
        .with_shutdown(Arc::clone(&shutdown));

    let run = mgr
        .prepare_adapter("codex", "fix bug", local_adapter())
        .unwrap();
    assert_eq!(mgr.logs(&run.id, false, 4096).unwrap(), "");
    assert_eq!(mgr.logs(&run.id, true, 4096).unwrap(), "");

    // Refresh and send must reject local (non-cloud) adapters and empty messages.
    let refresh_err = mgr.refresh(&run.id).unwrap_err().to_string();
    assert!(
        refresh_err.contains("only for cloud tasks"),
        "{refresh_err}"
    );

    let empty_send_err = mgr.send(&run.id, "   ").unwrap_err().to_string();
    assert!(
        empty_send_err.contains("message must not be empty"),
        "{empty_send_err}"
    );

    let local_send_err = mgr.send(&run.id, "continue").unwrap_err().to_string();
    assert!(
        local_send_err.contains("send is for cloud tasks"),
        "{local_send_err}"
    );

    // Logs tailing and credential redaction.
    let run_dir = mgr.run_dir(&run.id).unwrap();
    fs::write(
        run_dir.join("stdout.log"),
        "HEADER-12345\nAuthorization: Bearer sk-secret1234567890abcdef\nDONE",
    )
    .unwrap();
    fs::write(
        run_dir.join("stderr.log"),
        "ERR-PREFIX\napi_key=sk-abcdef1234567890abcdef",
    )
    .unwrap();

    let full_stdout = mgr.logs(&run.id, false, 4096).unwrap();
    assert!(!full_stdout.contains("sk-secret1234567890abcdef"));
    assert!(full_stdout.ends_with("DONE"));

    let tailed_stdout = mgr.logs(&run.id, false, 4).unwrap();
    assert_eq!(tailed_stdout, "DONE");

    let full_stderr = mgr.logs(&run.id, true, 4096).unwrap();
    assert!(!full_stderr.contains("sk-abcdef1234567890abcdef"));

    // mark_launch_failed transitions Queued -> Failed and persists error.
    mgr.mark_launch_failed(&run.id, "spawn error: permission denied")
        .unwrap();
    let failed = mgr.read(&run.id).unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("spawn error: permission denied")
    );

    // Shutdown flag cancels a queued run before dispatch.
    let queued = mgr
        .prepare_adapter("codex", "second run", local_adapter())
        .unwrap();
    shutdown.store(true, Ordering::Release);
    let cancelled = mgr.execute(&queued.id).unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn configure_adapter_and_reset_roundtrip_custom_command_adapter_on_disk() {
    let root = temp_workspace("configure-reset");
    let mgr = AgentManager::with_config(&root, CatalogKind::Execution, root.join("cfg"))
        .unwrap()
        .with_catalog(vec![test_def("codex", "CodexAgent", 1)]);

    let custom = Adapter::Command {
        program: "custom-codex".into(),
        args: vec!["--quiet".into()],
    };
    mgr.configure("codex", &custom).unwrap();
    let loaded = mgr.adapter("codex").unwrap();
    assert!(matches!(
        loaded,
        Adapter::Command { program, args }
            if program == "custom-codex" && args == vec!["--quiet".to_string()]
    ));

    mgr.reset("codex").unwrap();
    let reverted = mgr.adapter("codex").unwrap();
    assert!(reverted.is_cloud());

    let _ = fs::remove_dir_all(&root);
}
