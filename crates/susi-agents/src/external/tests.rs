use super::*;

struct Fixture {
    manager: AgentManager,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::for_kind(CatalogKind::Execution)
    }
    fn for_kind(kind: CatalogKind) -> Self {
        let root = std::env::temp_dir().join(format!("susi-agent-test-{}", unique_id().unwrap()));
        fs::create_dir_all(&root).unwrap();
        Self {
            manager: AgentManager::with_config(&root, kind, root.join("host-config")).unwrap(),
            root,
        }
    }
    #[cfg(unix)]
    fn command(&self, script: &str) -> Adapter {
        Adapter::Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into(), "test".into(), "{prompt}".into()],
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn curated_execution_agents_with_native_adapters() {
    let catalog = catalog(CatalogKind::Execution).unwrap();
    assert_eq!(catalog.len(), 18);
    let mut ids = std::collections::HashSet::new();
    for (index, agent) in catalog.iter().enumerate() {
        assert!(ids.insert(&agent.id));
        assert_eq!(agent.rank as usize, index + 1);
        agent.adapter.validate().unwrap();
        assert_eq!(
            definition(CatalogKind::Execution, &agent.peer_name)
                .unwrap()
                .id,
            agent.id
        );
    }
    assert!(matches!(
        definition(CatalogKind::Execution, "qwen-agent")
            .unwrap()
            .adapter,
        Adapter::Qwen { .. }
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "devin").unwrap().adapter,
        Adapter::Devin { .. }
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "manus").unwrap().adapter,
        Adapter::Manus { .. }
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "kilo-code")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "kilo"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "gemini-cli")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "gemini"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "aider")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "aider"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "swe-agent")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "sweagent"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "openclaw")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "openclaw"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "browser-use")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "browser-use"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "openviking")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "ov"
    ));
    assert!(matches!(
        definition(CatalogKind::Execution, "deerflow")
            .unwrap()
            .adapter,
        Adapter::Command { program, .. } if program == "deerflow"
    ));
}

#[test]
fn ten_unique_frameworks_with_python_adapters() {
    let catalog = catalog(CatalogKind::Framework).unwrap();
    assert_eq!(catalog.len(), 15);
    let mut ids = std::collections::HashSet::new();
    for (index, engine) in catalog.iter().enumerate() {
        assert!(ids.insert(&engine.id));
        assert_eq!(engine.rank as usize, index + 1);
        engine.adapter.validate().unwrap();
        assert!(matches!(engine.adapter, Adapter::Python { .. }));
        assert_eq!(
            definition(CatalogKind::Framework, &engine.peer_name)
                .unwrap()
                .id,
            engine.id
        );
    }
    // Distinct from the coding-executor Qwen peer.
    assert_ne!(
        definition(CatalogKind::Framework, "qwen-agent-engine")
            .unwrap()
            .peer_name,
        "QwenAgent"
    );
    let (kind, def) = resolve_managed("LangGraphEngine").unwrap();
    assert_eq!(kind, CatalogKind::Framework);
    assert_eq!(def.id, "langgraph");
    let (kind, def) = resolve_managed("ClaudeCodeAgent").unwrap();
    assert_eq!(kind, CatalogKind::Execution);
    assert_eq!(def.id, "claude-code");
}

#[test]
fn configuration_and_identifiers_fail_closed() {
    let fixture = Fixture::new();
    assert!(fixture.manager.read("../config").is_err());
    assert!(fixture
        .manager
        .configure(
            "invented",
            &Adapter::Qwen {
                python: "python3".into()
            }
        )
        .is_err());
    fixture
        .manager
        .configure(
            "codex",
            &Adapter::Command {
                program: "custom-codex".into(),
                args: vec![],
            },
        )
        .unwrap();
    assert!(
        matches!(fixture.manager.adapter("codex").unwrap(), Adapter::Command { program, .. } if program == "custom-codex")
    );
    fs::write(fixture.root.join("host-config/codex.json"), b"broken").unwrap();
    assert!(fixture.manager.adapter("codex").is_err());
    fixture.manager.reset("codex").unwrap();
    assert!(
        matches!(fixture.manager.adapter("codex").unwrap(), Adapter::Command { program, .. } if program == "codex")
    );
}

#[test]
#[cfg(unix)]
fn process_success_failure_and_literal_prompt_survive_manager_restart() {
    let fixture = Fixture::new();
    let prompt = "$(touch INJECTED) ; `echo bad` {workspace}\nquoted ' \"";
    for (script, expected) in [
        ("printf '%s' \"$1\"; printf error >&2", RunStatus::Succeeded),
        ("printf '%s' \"$1\"; exit 7", RunStatus::Failed),
    ] {
        let run = fixture
            .manager
            .prepare_adapter("codex", prompt, fixture.command(script))
            .unwrap();
        let finished = fixture.manager.execute(&run.id).unwrap();
        assert_eq!(finished.status, expected);
        let reopened = AgentManager::with_config(
            &fixture.root,
            CatalogKind::Execution,
            fixture.root.join("host-config"),
        )
        .unwrap();
        assert_eq!(reopened.status(&run.id).unwrap().status, expected);
        assert_eq!(reopened.logs(&run.id, false, 65536).unwrap(), prompt);
        assert!(fixture.manager.execute(&run.id).is_err());
    }
    assert!(!fixture.root.join("INJECTED").exists());
    assert_eq!(fixture.manager.list().unwrap().len(), 2);
}

#[test]
#[cfg(unix)]
fn cancellation_reaps_owned_process_and_preserves_terminal_state() {
    let fixture = Fixture::new();
    let run = fixture
        .manager
        .prepare_adapter("codex", "wait", fixture.command("echo started; sleep 30"))
        .unwrap();
    let manager = fixture.manager.clone();
    let id = run.id.clone();
    let worker = std::thread::spawn(move || manager.execute(&id).unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fixture.manager.read(&run.id).unwrap().pid.is_none() {
        assert!(std::time::Instant::now() < deadline, "worker did not start");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let pid = fixture.manager.read(&run.id).unwrap().pid.unwrap();
    assert!(fixture.manager.execute(&run.id).is_err());
    fixture.manager.cancel(&run.id).unwrap();
    assert_eq!(worker.join().unwrap().status, RunStatus::Cancelled);
    // SAFETY: probes the former child PID; no pointers or mutation.
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert_eq!(
        fixture.manager.cancel(&run.id).unwrap().status,
        RunStatus::Cancelled
    );
}

#[test]
#[cfg(unix)]
fn queued_cancel_and_lost_worker_do_not_dispatch_or_claim_success() {
    let fixture = Fixture::new();
    let run = fixture
        .manager
        .prepare_adapter("codex", "wait", fixture.command("touch UNEXPECTED"))
        .unwrap();
    assert_eq!(
        fixture.manager.cancel(&run.id).unwrap().status,
        RunStatus::Cancelled
    );
    assert!(fixture.manager.execute(&run.id).is_err());
    assert!(!fixture.root.join("UNEXPECTED").exists());
    let mut run = fixture
        .manager
        .prepare_adapter("codex", "wait", fixture.command("true"))
        .unwrap();
    run.status = RunStatus::Running;
    fixture.manager.save(&run).unwrap();
    assert_eq!(
        fixture.manager.status(&run.id).unwrap().status,
        RunStatus::Unknown
    );
    assert_eq!(
        fixture.manager.cancel(&run.id).unwrap().status,
        RunStatus::Unknown
    );
}

#[test]
#[cfg(unix)]
fn python_framework_runner_invokes_absolute_script() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let script = fixture.root.join("entry.py");
    fs::write(
        &script,
        "import os\nprint('FRAMEWORK:' + os.environ['SUSI_AGENT_PROMPT'])\n",
    )
    .unwrap();
    let config = fixture.root.join("langgraph.json");
    fs::write(
        &config,
        serde_json::json!({"script": script.to_string_lossy()}).to_string(),
    )
    .unwrap();
    std::env::set_var("SUSI_LANGGRAPH_CONFIG", &config);
    let adapter = Adapter::Python {
        python: "python3".into(),
        import_name: "json".into(),
        config_env: "SUSI_LANGGRAPH_CONFIG".into(),
    };
    let run = fixture
        .manager
        .prepare_adapter("langgraph", "hello-engine", adapter)
        .unwrap();
    let finished = fixture.manager.execute(&run.id).unwrap();
    assert_eq!(finished.status, RunStatus::Succeeded);
    assert!(fixture
        .manager
        .logs(&run.id, false, 65536)
        .unwrap()
        .contains("FRAMEWORK:hello-engine"));
    std::env::remove_var("SUSI_LANGGRAPH_CONFIG");
}
