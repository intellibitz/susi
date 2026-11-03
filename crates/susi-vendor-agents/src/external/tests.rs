use super::*;

/// A synthetic catalog row in the pack format (core bundles no vendor
/// catalog; commercial packs ship the real ones).
fn synthetic(id: &str, peer: &str, rank: u8, program: &str) -> AgentDefinition {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": id,
        "peer_name": peer,
        "rank": rank,
        "documentation": "https://example.invalid",
        "adapter": {"kind": "command", "program": program, "args": ["{prompt}"]},
    }))
    .unwrap()
}

fn synthetic_catalog(kind: CatalogKind) -> Vec<AgentDefinition> {
    match kind {
        CatalogKind::Execution => vec![
            synthetic("codex", "CodexAgent", 1, "codex"),
            synthetic("aider", "AiderAgent", 2, "aider"),
            synthetic("a2a", "A2aAgent", 3, "a2a"),
        ],
        CatalogKind::Framework => vec![synthetic("langgraph", "LangGraphEngine", 1, "python3")],
    }
}

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
            manager: AgentManager::with_config(&root, kind, root.join("host-config"))
                .unwrap()
                .with_catalog(synthetic_catalog(kind)),
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

/// Zero-UI plan A9: core bundles no agent catalog. What is installed is the
/// active pack's entries plus the adapter files the host writes into the
/// manager's config dir; bookkeeping such as `ready.json` is not an adapter.
#[test]
fn installed_catalog_admits_host_adapter_files() {
    let root = std::env::temp_dir().join(format!("susi-agent-host-{}", unique_id().unwrap()));
    let empty = root.join("empty");
    let host = root.join("host");
    fs::create_dir_all(&empty).unwrap();
    fs::create_dir_all(&host).unwrap();
    let baseline = catalog_with_host_dir(CatalogKind::Execution, &empty).unwrap();
    fs::write(
        host.join("acme-coder.json"),
        r#"{"kind":"command","program":"acme","args":["{prompt}"]}"#,
    )
    .unwrap();
    fs::write(host.join("ready.json"), "[]").unwrap();
    let installed = catalog_with_host_dir(CatalogKind::Execution, &host).unwrap();
    assert_eq!(installed.len(), baseline.len() + 1);
    let acme = installed.iter().find(|a| a.id == "acme-coder").unwrap();
    assert_eq!(acme.peer_name, "acme-coder");
    assert_eq!(acme.rank, u8::MAX);
    assert!(matches!(&acme.adapter, Adapter::Command { program, .. } if program == "acme"));
    let manager = AgentManager::with_config(&root, CatalogKind::Execution, host.clone()).unwrap();
    assert!(manager.adapter("acme-coder").is_ok());
    assert!(manager.adapter("ready").is_err());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn fixed_catalog_resolves_ids_and_peer_names() {
    let fixture = Fixture::new();
    for agent in synthetic_catalog(CatalogKind::Execution) {
        agent.adapter.validate().unwrap();
        assert!(fixture.manager.adapter(&agent.peer_name).is_ok());
    }
    let err = fixture.manager.adapter("unknown-agent").unwrap_err();
    assert!(err.to_string().contains("extension pack"), "{err}");
    let frameworks = Fixture::for_kind(CatalogKind::Framework);
    assert!(frameworks.manager.adapter("LangGraphEngine").is_ok());
    assert!(frameworks.manager.adapter("codex").is_err());
}

#[test]
fn configuration_and_identifiers_fail_closed() {
    let fixture = Fixture::new();
    assert!(fixture.manager.read("../config").is_err());
    assert!(fixture
        .manager
        .configure(
            "invented",
            &Adapter::Command {
                program: "custom-invented".into(),
                args: vec![],
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
        .unwrap()
        .with_catalog(synthetic_catalog(CatalogKind::Execution));
        assert_eq!(reopened.status(&run.id).unwrap().status, expected);
        assert_eq!(reopened.logs(&run.id, false, 65536).unwrap(), prompt);
        assert!(fixture.manager.execute(&run.id).is_err());
    }
    assert!(!fixture.root.join("INJECTED").exists());
    assert_eq!(fixture.manager.list().unwrap().len(), 2);
}

#[test]
#[cfg(unix)]
fn agent_run_cannot_rewrite_workspace_git_identity() {
    // Regression: aider under an isolated HOME wrote `Your Name` /
    // `you@example.com` into the operator's repo-local config (EV-CLAUDE-001).
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&fixture.root)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["config", "--local", "user.name", "Operator"]);
    git(&["config", "--local", "user.email", "op@example.test"]);
    let script = "git config user.name 'Your Name'; git config user.email you@example.com; \
                  printf '%s <%s>' \"$GIT_AUTHOR_NAME\" \"$GIT_COMMITTER_EMAIL\"";
    let run = fixture
        .manager
        .prepare_adapter("aider", "edit", fixture.command(script))
        .unwrap();
    let finished = fixture.manager.execute(&run.id).unwrap();
    assert_eq!(finished.status, RunStatus::Succeeded);
    let error = finished.error.unwrap_or_default();
    assert!(error.contains("altered repo-local git identity"), "{error}");
    assert_eq!(
        git(&["config", "--local", "--get", "user.name"]),
        "Operator"
    );
    assert_eq!(
        git(&["config", "--local", "--get", "user.email"]),
        "op@example.test"
    );
    assert_eq!(
        fixture.manager.logs(&run.id, false, 4096).unwrap(),
        "Operator <op@example.test>"
    );
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
fn framework_runner_invokes_configured_command() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let adapter = fixture.command("printf 'FRAMEWORK:%s\\n' \"$SUSI_AGENT_PROMPT\"");
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
}

#[cfg(unix)]
#[test]
fn failed_task_reports_stderr_cause_redacted() {
    let fx = Fixture::new();
    let adapter = fx.command("echo 'Workspace Trust Required' >&2; echo more >&2; echo 'token sk-abcdef1234567890abcd' >&2; exit 3");
    let run = fx.manager.prepare_adapter("aider", "x", adapter).unwrap();
    let done = fx.manager.execute(&run.id).unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.exit_code, Some(3));
    let err = done.error.unwrap();
    assert!(err.starts_with("Workspace Trust Required"), "{err}");
    assert!(!err.contains("abcdef1234567890"), "{err}");
}
