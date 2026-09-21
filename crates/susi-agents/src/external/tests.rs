use super::*;

struct Fixture {
    manager: AgentManager,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("susi-agent-test-{}", unique_id().unwrap()));
        fs::create_dir_all(&root).unwrap();
        Self {
            manager: AgentManager::with_config(&root, root.join("host-config")).unwrap(),
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
fn ten_unique_agents_with_native_adapters() {
    let catalog = catalog().unwrap();
    assert_eq!(catalog.len(), 10);
    let mut ids = std::collections::HashSet::new();
    for (index, agent) in catalog.iter().enumerate() {
        assert!(ids.insert(&agent.id));
        assert_eq!(agent.rank as usize, index + 1);
        agent.adapter.validate().unwrap();
        assert_eq!(definition(&agent.peer_name).unwrap().id, agent.id);
    }
    assert!(matches!(
        definition("qwen-agent").unwrap().adapter,
        Adapter::Qwen { .. }
    ));
    assert!(matches!(
        definition("devin").unwrap().adapter,
        Adapter::Devin { .. }
    ));
    assert!(matches!(
        definition("manus").unwrap().adapter,
        Adapter::Manus { .. }
    ));
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
        let reopened =
            AgentManager::with_config(&fixture.root, fixture.root.join("host-config")).unwrap();
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
