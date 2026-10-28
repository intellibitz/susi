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
    assert_eq!(catalog.len(), 20);
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

#[cfg(unix)]
#[test]
fn failed_task_reports_stderr_cause_redacted() {
    let fx = Fixture::new();
    let adapter = fx.command("echo 'Workspace Trust Required' >&2; echo more >&2; echo 'token sk-abcdef1234567890abcd' >&2; exit 3");
    let run = fx.manager.prepare_adapter("cursor", "x", adapter).unwrap();
    let done = fx.manager.execute(&run.id).unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.exit_code, Some(3));
    let err = done.error.unwrap();
    assert!(err.starts_with("Workspace Trust Required"), "{err}");
    assert!(!err.contains("abcdef1234567890"), "{err}");
}

#[test]
fn cursor_adapter_trusts_the_workspace_for_headless_runs() {
    // cursor-agent refuses `-p` in an untrusted directory unless told to trust it.
    match definition(CatalogKind::Execution, "cursor")
        .unwrap()
        .adapter
    {
        Adapter::Command { program, args } => {
            assert_eq!(program, "cursor-agent");
            assert!(args.iter().any(|a| a == "--trust"));
            assert_eq!(args.last().map(String::as_str), Some("{prompt}"));
        }
        other => panic!("expected command adapter, got {other:?}"),
    }
}

/// Write an executable script from a child process. Written from this one, its
/// descriptor is open in every child another test thread forks until that
/// child execs, and running the script meanwhile fails with "Text file busy".
#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::io::Write;
    let mut sh = std::process::Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    sh.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
    assert!(sh.wait().unwrap().success(), "write {}", path.display());
}

/// A `python3` that logs each interpreter start, so a test can count them.
#[cfg(unix)]
fn counting_python(root: &Path) -> (PathBuf, PathBuf) {
    let log = root.join("python-starts.log");
    let wrapper = root.join("python3");
    write_executable(
        &wrapper,
        &format!(
            "#!/bin/sh\necho start >> '{}'\nexec python3 \"$@\"\n",
            log.display()
        ),
    );
    (wrapper, log)
}

#[cfg(unix)]
fn starts(log: &Path) -> usize {
    fs::read_to_string(log).map_or(0, |s| s.lines().count())
}

/// The readiness scan used to start one interpreter per Python framework,
/// sixteen on every susi command. It now probes all of a scan's imports in
/// one interpreter, with the verdict the single `import` probe gives.
#[test]
#[cfg(unix)]
fn python_imports_are_probed_in_one_interpreter_per_scan() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let (python, log) = counting_python(&fixture.root);
    let config = fixture.root.join("engine.json");
    fs::write(&config, "{}").unwrap();
    std::env::set_var("SUSI_PYIMPORT_SCAN_CONFIG", &config);
    let adapter = |module: &str| Adapter::Python {
        python: python.to_string_lossy().into_owned(),
        import_name: module.into(),
        config_env: "SUSI_PYIMPORT_SCAN_CONFIG".into(),
    };
    let adapters = vec![
        adapter("json"),
        adapter("susi_no_such_module_xyz"),
        adapter("os.path"),
    ];
    let verdicts = catalog::with_python_imports(&adapters, || {
        adapters
            .iter()
            .map(|a| a.preflight().map(|_| ()).map_err(|e| e.to_string()))
            .collect::<Vec<_>>()
    });
    assert_eq!(starts(&log), 1, "one interpreter for the whole scan");
    assert!(verdicts[0].is_ok(), "{:?}", verdicts[0]);
    assert!(
        verdicts[1]
            .as_ref()
            .unwrap_err()
            .contains("not importable: susi_no_such_module_xyz"),
        "{:?}",
        verdicts[1]
    );
    assert!(
        verdicts[2].is_ok(),
        "a dotted module imports as `import os.path` does"
    );

    // Outside a scan nothing is remembered: a framework installed after the
    // last scan must be seen, so a preflight probes on its own again.
    assert!(adapters[0].preflight().is_ok());
    assert_eq!(starts(&log), 2);
    std::env::remove_var("SUSI_PYIMPORT_SCAN_CONFIG");
}

/// An interpreter that cannot finish the batch probe leaves every module to
/// the single probe, so the scan never reports less than it did before.
#[test]
#[cfg(unix)]
fn python_imports_fall_back_to_single_probes_when_the_batch_fails() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let log = fixture.root.join("starts.log");
    // Fails any run with more than one module (the batch); answers a single
    // `-c "import json"` the way python3 does.
    let python = fixture.root.join("python3");
    write_executable(
        &python,
        &format!(
            "#!/bin/sh\necho start >> '{}'\n[ $# -gt 2 ] && exit 9\nexec python3 \"$@\"\n",
            log.display()
        ),
    );
    let config = fixture.root.join("engine.json");
    fs::write(&config, "{}").unwrap();
    std::env::set_var("SUSI_PYIMPORT_FALLBACK_CONFIG", &config);
    let adapter = |module: &str| Adapter::Python {
        python: python.to_string_lossy().into_owned(),
        import_name: module.into(),
        config_env: "SUSI_PYIMPORT_FALLBACK_CONFIG".into(),
    };
    let adapters = vec![adapter("json"), adapter("susi_no_such_module_xyz")];
    let verdicts = catalog::with_python_imports(&adapters, || {
        adapters
            .iter()
            .map(|a| a.preflight().is_ok())
            .collect::<Vec<_>>()
    });
    assert_eq!(verdicts, vec![true, false]);
    assert_eq!(
        starts(&log),
        3,
        "one failed batch, then one probe per module"
    );
    std::env::remove_var("SUSI_PYIMPORT_FALLBACK_CONFIG");
}

/// A `python3` that also finds the fixture modules in `dir`.
#[cfg(unix)]
fn python_with_modules(root: &Path, modules: &[(&str, &str)]) -> PathBuf {
    let dir = root.join("pymods");
    fs::create_dir_all(&dir).unwrap();
    for (name, source) in modules {
        fs::write(dir.join(format!("{name}.py")), source).unwrap();
    }
    let wrapper = root.join("python3-with-modules");
    write_executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nPYTHONPATH='{}' exec python3 \"$@\"\n",
            dir.display()
        ),
    );
    wrapper
}

/// What an import prints, what state it leaves, how it ends and how long it
/// takes must not change a verdict or lose the others: each module gets the
/// answer `python -c "import X"` gives (`susi_binary_noise` after
/// `susi_swaps_stdout` imports as it would alone), and one that hangs costs
/// one per-import bound.
#[test]
#[cfg(unix)]
fn python_import_batch_survives_hostile_imports() {
    use catalog::ImportVerdict::{Importable, NotImportable, TimedOut};
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let python = python_with_modules(
        &fixture.root,
        &[
            ("susi_noisy", "import sys\nsys.stdout.write('loading...')\n"),
            (
                "susi_swaps_stdout",
                "import io, sys\nsys.stdout = io.StringIO()\nprint('hidden')\n",
            ),
            (
                "susi_binary_noise",
                "import sys\nsys.stdout.buffer.write(b'\\xff\\xfe\\n')\nsys.stdout.flush()\n",
            ),
            ("susi_exit_zero", "raise SystemExit(0)\n"),
            ("susi_exit_three", "raise SystemExit(3)\n"),
            ("susi_hangs", "import time\ntime.sleep(60)\n"),
            ("susi_ends_process", "import os\nos._exit(0)\n"),
            ("susi_raises", "raise ImportError('missing dependency')\n"),
            // On the path ahead of the standard library, as a file in the
            // working directory would be: the batch must not need it.
            ("importlib", "raise ImportError('shadowed importlib')\n"),
            // Importable under `python -c` only if the importing frame, the
            // `__main__` namespace and the descriptors look as they do there.
            (
                "susi_inspects_importer",
                "import os, sys, __main__\n\
                 f = sys._getframe(1)\n\
                 while f.f_code.co_filename.startswith('<frozen'):\n    f = f.f_back\n\
                 assert f.f_globals['__name__'] == '__main__', 'importer namespace'\n\
                 assert not [n for n in vars(__main__) if not n.startswith('__')], 'batch names'\n\
                 try:\n    os.fstat(3)\nexcept OSError:\n    pass\n\
                 else:\n    raise ImportError('a descriptor beyond 0-2 is open')\n",
            ),
        ],
    );
    let modules: Vec<String> = [
        "json",
        "susi_noisy",
        "susi_swaps_stdout",
        "susi_binary_noise",
        "susi_exit_zero",
        "susi_exit_three",
        "susi_hangs",
        "os.path",
        "susi_ends_process",
        "susi_raises",
        "susi_no_such_module_xyz",
        "susi_inspects_importer",
    ]
    .iter()
    .map(|m| (*m).to_string())
    .collect();
    let started = std::time::Instant::now();
    let verdicts = catalog::probe_imports(&python, &modules, std::time::Duration::from_secs(2));
    let elapsed = started.elapsed();
    let get = |m: &str| verdicts.get(m).copied();
    assert_eq!(get("json"), Some(Importable));
    assert_eq!(get("susi_noisy"), Some(Importable), "unterminated output");
    assert_eq!(
        get("susi_swaps_stdout"),
        Some(Importable),
        "replaced sys.stdout"
    );
    assert_eq!(
        get("susi_binary_noise"),
        Some(Importable),
        "non-UTF-8 output"
    );
    assert_eq!(get("susi_exit_zero"), Some(Importable), "exit 0 is success");
    assert_eq!(get("susi_exit_three"), Some(NotImportable));
    assert_eq!(get("susi_hangs"), Some(TimedOut));
    assert_eq!(get("os.path"), Some(Importable), "answered after a hang");
    assert_eq!(
        get("susi_ends_process"),
        Some(Importable),
        "os._exit(0) exits 0, as `python -c` reports it"
    );
    assert_eq!(get("susi_raises"), Some(NotImportable));
    assert_eq!(get("susi_no_such_module_xyz"), Some(NotImportable));
    assert_eq!(
        get("susi_inspects_importer"),
        Some(Importable),
        "imported from a namespace and descriptors other than `python -c`'s"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "one hang costs one bound, not one per module: {elapsed:?}"
    );
}

/// A scan's verdict decides the preflight inside that scan and is gone after
/// it, on every platform (the batch probe itself needs `os.fork`, so on Windows
/// every module takes the single probe). The test binary stands in for the
/// interpreter: a stored verdict means nothing is run.
#[test]
fn python_imports_decide_preflight_only_inside_their_scan() {
    use catalog::ImportVerdict::{Importable, NotImportable, TimedOut};
    let program = std::env::current_exe().unwrap();
    let adapter = Adapter::Python {
        python: program.to_string_lossy().into_owned(),
        import_name: "susi_scoped_module".into(),
        config_env: "SUSI_PYIMPORT_SCOPE_UNSET_CONFIG".into(),
    };
    let preflight_in_scan = |verdict| {
        catalog::with_scan_verdicts_for_test(&program, "susi_scoped_module", verdict, || {
            adapter.preflight().unwrap_err().to_string()
        })
    };
    let err = preflight_in_scan(NotImportable);
    assert!(err.contains("not importable: susi_scoped_module"), "{err}");
    // A stuck import fails the way the single probe's timeout did: as a failed
    // probe, not as a missing package.
    let err = preflight_in_scan(TimedOut);
    assert!(
        err.contains("failed to probe import susi_scoped_module") && err.contains("did not finish"),
        "{err}"
    );
    // Importable passes the import check; the config check decides next.
    let err = preflight_in_scan(Importable);
    assert!(err.contains("SUSI_PYIMPORT_SCOPE_UNSET_CONFIG"), "{err}");
    assert_eq!(
        catalog::scan_import_verdict(&program, "susi_scoped_module"),
        None,
        "a verdict outlived its scan"
    );
}

/// The change that matters is the scan's: sixteen interpreters per scan down
/// to one. Every Python framework the catalog ships, pointed at a counting
/// interpreter, is probed by a single start.
#[test]
#[cfg(unix)]
fn python_imports_the_catalog_scan_starts_one_interpreter() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let (python, log) = counting_python(&fixture.root);
    let mut python_ids = Vec::new();
    for def in catalog(CatalogKind::Framework).unwrap() {
        if let Adapter::Python { import_name, .. } = &def.adapter {
            fixture
                .manager
                .configure(
                    &def.id,
                    &Adapter::Python {
                        python: python.to_string_lossy().into_owned(),
                        import_name: import_name.clone(),
                        // Unset: the import check passes or fails, and the
                        // config check keeps every entry from being registered.
                        config_env: "SUSI_PYIMPORT_SCAN_UNSET_CONFIG".into(),
                    },
                )
                .unwrap();
            python_ids.push(def.id.clone());
        }
    }
    assert!(python_ids.len() > 2, "the catalog ships Python frameworks");
    let ready = fixture.manager.scan_ready().unwrap();
    assert_eq!(
        starts(&log),
        1,
        "one interpreter for the {} Python frameworks of the scan",
        python_ids.len()
    );
    assert!(
        ready
            .iter()
            .all(|r| !python_ids.iter().any(|id| r["id"] == id.as_str())),
        "{ready:?}"
    );
}

/// Whether process `pid` has exited (gone, or a zombie nobody reaped yet)
/// within `wait`.
#[cfg(unix)]
fn gone_within(pid: &str, wait: std::time::Duration) -> bool {
    let until = std::time::Instant::now() + wait;
    loop {
        let stat = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if stat.is_empty() || stat.starts_with('Z') {
            return true;
        }
        if std::time::Instant::now() > until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The interpreter itself can stop answering (stopped by an import) or die
/// (killed by one). Either way the import it was waiting on must not keep
/// running unwatched, and the modules after it still get a batch; a batch
/// that dies before deciding anything is not restarted module by module.
/// The interpreter here is slow to resume after each fork, as a loaded
/// machine makes it: the import acts first, and the caller must already know
/// which module is in flight.
#[test]
#[cfg(unix)]
fn python_import_batch_survives_a_stopped_or_killed_interpreter() {
    use catalog::ImportVerdict::{Importable, TimedOut};
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let pidfile = fixture.root.join("stopper.pid");
    let python = python_with_modules(
        &fixture.root,
        &[
            (
                "sitecustomize",
                "import os, time\nfork = os.fork\n\
                 def slow():\n    pid = fork()\n    if pid:\n        time.sleep(0.2)\n    \
                 return pid\nos.fork = slow\n",
            ),
            (
                "susi_stops_parent",
                &format!(
                    "import os, signal, time\nopen('{}', 'w').write(str(os.getpid()))\n\
                     os.kill(os.getppid(), signal.SIGSTOP)\ntime.sleep(60)\n",
                    pidfile.display()
                ),
            ),
            ("susi_kills_parent", "import os\nos.kill(os.getppid(), 9)\n"),
        ],
    );
    let names = |ms: &[&str]| ms.iter().map(|m| (*m).to_string()).collect::<Vec<_>>();
    let bound = std::time::Duration::from_secs(1);

    let verdicts = catalog::probe_imports(
        &python,
        &names(&["json", "susi_stops_parent", "os.path"]),
        bound,
    );
    assert_eq!(verdicts.get("json"), Some(&Importable));
    assert_eq!(verdicts.get("susi_stops_parent"), Some(&TimedOut));
    assert_eq!(verdicts.get("os.path"), Some(&Importable), "answered after");
    let stuck = fs::read_to_string(&pidfile).unwrap().trim().to_string();
    assert!(
        gone_within(&stuck, std::time::Duration::from_secs(3)),
        "the import the stopped interpreter was waiting on still runs"
    );

    let verdicts = catalog::probe_imports(
        &python,
        &names(&["json", "susi_kills_parent", "os.path"]),
        bound,
    );
    assert_eq!(verdicts.get("json"), Some(&Importable));
    assert_eq!(
        verdicts.get("susi_kills_parent"),
        None,
        "left to the single probe"
    );
    assert_eq!(verdicts.get("os.path"), Some(&Importable), "answered after");

    let verdicts = catalog::probe_imports(&python, &names(&["susi_kills_parent", "json"]), bound);
    assert!(
        verdicts.is_empty(),
        "a batch that decided nothing is not retried: {verdicts:?}"
    );
}

/// The batch's own process group keeps a terminal's Ctrl-C from reaching it.
/// When the process that started it ends, the batch must stop the import in
/// flight and exit, not leave it running out its bound unwatched. The caller
/// here is a shell that starts the batch, waits until the import runs, and
/// exits.
#[test]
#[cfg(unix)]
fn python_import_batch_ends_with_its_caller() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let pidfile = fixture.root.join("import.pid");
    let python = python_with_modules(
        &fixture.root,
        &[(
            "susi_hangs_with_pid",
            &format!(
                "import os, time\nopen('{}', 'w').write(str(os.getpid()))\ntime.sleep(60)\n",
                pidfile.display()
            ),
        )],
    );
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(
            "\"$1\" -c \"$2\" token 30 susi_hangs_with_pid >/dev/null 2>&1 &\n\
             echo $!\n\
             i=0\n\
             while [ ! -s \"$3\" ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i + 1)); done\n",
        )
        .arg("sh")
        .arg(&python)
        .arg(catalog::BATCH_SCRIPT)
        .arg(&pidfile)
        .output()
        .unwrap();
    let batch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let import = fs::read_to_string(&pidfile)
        .expect("the import never started")
        .trim()
        .to_string();
    let wait = std::time::Duration::from_secs(3);
    assert!(gone_within(&batch, wait), "the batch outlived its caller");
    assert!(
        gone_within(&import, wait),
        "the import outlived the batch's caller"
    );
}

/// A fork the system refuses, on the first module or a later one, leaves the
/// undecided modules to the single probe without starting another batch.
#[test]
#[cfg(unix)]
fn python_import_batch_stops_when_a_fork_is_refused() {
    use catalog::ImportVerdict::Importable;
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let (counting, log) = counting_python(&fixture.root);
    let refuse_after = |forks: usize| {
        let dir = fixture.root.join(format!("refuse-after-{forks}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("sitecustomize.py"),
            format!(
                "import errno, os\nforks, fork = [0], os.fork\n\
                 def refusing():\n    forks[0] += 1\n    \
                 if forks[0] > {forks}:\n        raise OSError(errno.EAGAIN, 'refused')\n    \
                 return fork()\nos.fork = refusing\n"
            ),
        )
        .unwrap();
        let wrapper = dir.join("python3");
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nPYTHONPATH='{}' exec '{}' \"$@\"\n",
                dir.display(),
                counting.display()
            ),
        );
        wrapper
    };
    let modules: Vec<String> = ["json", "os.path", "susi_no_such_module_xyz"]
        .iter()
        .map(|m| (*m).to_string())
        .collect();
    let bound = std::time::Duration::from_secs(5);

    let verdicts = catalog::probe_imports(&refuse_after(0), &modules, bound);
    assert!(verdicts.is_empty(), "{verdicts:?}");
    assert_eq!(starts(&log), 1, "a refused fork starts no second batch");

    let verdicts = catalog::probe_imports(&refuse_after(1), &modules, bound);
    assert_eq!(verdicts.get("json"), Some(&Importable));
    assert_eq!(
        verdicts.len(),
        1,
        "the rest go to the single probe: {verdicts:?}"
    );
    assert_eq!(starts(&log), 2, "a refused fork starts no second batch");
}

/// Whatever the interpreter prints before answering (a sitecustomize, a
/// startup hook), unterminated and not UTF-8, cannot hide or garble an answer.
#[test]
#[cfg(unix)]
fn python_import_batch_reads_through_interpreter_noise() {
    use catalog::ImportVerdict::{Importable, NotImportable};
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let python = python_with_modules(
        &fixture.root,
        &[(
            "sitecustomize",
            "import os\nos.write(1, b'\\xff\\xfe startup noise without a newline')\n",
        )],
    );
    let modules = vec!["json".to_string(), "susi_no_such_module_xyz".to_string()];
    let verdicts = catalog::probe_imports(&python, &modules, std::time::Duration::from_secs(5));
    assert_eq!(verdicts.get("json"), Some(&Importable));
    assert_eq!(
        verdicts.get("susi_no_such_module_xyz"),
        Some(&NotImportable)
    );
}
