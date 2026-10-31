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
    assert_eq!(catalog.len(), 15);
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
    assert_eq!(catalog.len(), 14);
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
    let (kind, def) = resolve_managed("CodexAgent").unwrap();
    assert_eq!(kind, CatalogKind::Execution);
    assert_eq!(def.id, "codex");
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
    let run = fx.manager.prepare_adapter("aider", "x", adapter).unwrap();
    let done = fx.manager.execute(&run.id).unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.exit_code, Some(3));
    let err = done.error.unwrap();
    assert!(err.starts_with("Workspace Trust Required"), "{err}");
    assert!(!err.contains("abcdef1234567890"), "{err}");
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
            // A startup hook that leaves a name in `__main__`, as some do.
            ("sitecustomize", "import __main__\n__main__.susi_hook = 1\n"),
            // Importable only if the importing frame, the `__main__`
            // namespace (the hook's name kept, the batch's gone) and
            // `sys.argv` look as they do under `python -c`.
            (
                "susi_inspects_importer",
                "import sys, __main__\n\
                 f = sys._getframe(1)\n\
                 while f.f_code.co_filename.startswith('<frozen'):\n    f = f.f_back\n\
                 assert f.f_globals['__name__'] == '__main__', 'importer namespace'\n\
                 names = [n for n in vars(__main__) if not n.startswith('__')]\n\
                 assert names == ['susi_hook'], names\n\
                 assert sys.argv == ['-c'], sys.argv\n",
            ),
            // Fails when descriptor 3 is open: the batch must leave the same
            // descriptors open as the single probe has, whatever this process
            // inherited.
            (
                "susi_counts_descriptors",
                "import os\n\
                 try:\n    os.fstat(3)\nexcept OSError:\n    pass\n\
                 else:\n    raise ImportError('descriptor 3 is open')\n",
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
        "susi_counts_descriptors",
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
        "imported from a frame, namespace or argv other than `python -c`'s"
    );
    let single = std::process::Command::new(&python)
        .args(["-c", "import susi_counts_descriptors"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
        .status
        .success();
    assert_eq!(
        get("susi_counts_descriptors"),
        Some(if single { Importable } else { NotImportable }),
        "the batch's import sees descriptors the single probe does not"
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
    let config = fixture.root.join("engine.json");
    fs::write(&config, "{}").unwrap();
    std::env::set_var("SUSI_PYIMPORT_SCAN_READY_CONFIG", &config);
    let mut python_ids = Vec::new();
    let mut ready_peer = None;
    for def in catalog(CatalogKind::Framework).unwrap() {
        if let Adapter::Python { import_name, .. } = &def.adapter {
            // The first entry imports `json` and finds its config: ready. The
            // rest keep their import and find no config: never registered.
            let (import_name, config_env) = if ready_peer.is_none() {
                ready_peer = Some(def.peer_name.clone());
                ("json".to_string(), "SUSI_PYIMPORT_SCAN_READY_CONFIG")
            } else {
                (import_name.clone(), "SUSI_PYIMPORT_SCAN_UNSET_CONFIG")
            };
            fixture
                .manager
                .configure(
                    &def.id,
                    &Adapter::Python {
                        python: python.to_string_lossy().into_owned(),
                        import_name,
                        config_env: config_env.into(),
                    },
                )
                .unwrap();
            python_ids.push(def.id.clone());
        }
    }
    assert!(python_ids.len() > 2, "the catalog ships Python frameworks");
    let mut registered = Vec::new();
    let ready = fixture
        .manager
        .scan_ready(&mut |cap| registered.push(cap.name))
        .unwrap();
    std::env::remove_var("SUSI_PYIMPORT_SCAN_READY_CONFIG");
    assert_eq!(
        starts(&log),
        1,
        "one interpreter for the {} Python frameworks of the scan",
        python_ids.len()
    );
    let ready_peers: Vec<&str> = ready
        .iter()
        .filter_map(|r| r["peer_name"].as_str())
        .collect();
    assert_eq!(
        registered, ready_peers,
        "every ready entry, and only those, is registered"
    );
    let python_ready: Vec<&serde_json::Value> = ready
        .iter()
        .filter(|r| python_ids.iter().any(|id| r["id"] == id.as_str()))
        .collect();
    assert_eq!(python_ready.len(), 1, "{python_ready:?}");
    assert_eq!(python_ready[0]["peer_name"].as_str(), ready_peer.as_deref());
}

/// Whether process `pid` has exited (gone, or a zombie nobody reaped yet)
/// within `wait`. Something that is not a pid, or a `ps` that cannot run,
/// fails the test rather than passing for "gone".
#[cfg(unix)]
fn gone_within(pid: &str, wait: std::time::Duration) -> bool {
    let until = std::time::Instant::now() + wait;
    loop {
        if process_state(pid).is_none_or(|state| state == 'Z') {
            return true;
        }
        if std::time::Instant::now() > until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The state letter of process `pid`, `None` once it is gone. On Linux it is
/// read from /proc (the field after the parenthesised command name, which may
/// itself hold parentheses); elsewhere from `ps`, which must run.
#[cfg(unix)]
fn process_state(pid: &str) -> Option<char> {
    assert!(
        !pid.is_empty() && !pid.starts_with('0') && pid.bytes().all(|b| b.is_ascii_digit()),
        "not a pid: {pid:?}"
    );
    if Path::new("/proc/self/stat").is_file() {
        let stat = fs::read_to_string(Path::new("/proc").join(pid).join("stat")).ok()?;
        let (_, rest) = stat.rsplit_once(')')?;
        return rest.trim_start().chars().next();
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("ps reports whether a process is gone");
    String::from_utf8_lossy(&out.stdout).trim().chars().next()
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
/// When the process that started it ends, the batch must end the import in
/// flight and everything it started, not leave them running out the bound or
/// for good. The caller here starts the batch as `run_import_batch` does (a
/// group of its own, a stdout pipe it reads, a null stdin), waits until the
/// import runs, and exits: straight to the interpreter, through a wrapper
/// script that runs python without exec'ing it, and under a startup hook that
/// restores SIGPIPE's default (which would kill the batch at its first write
/// to a broken pipe); with an import that started a helper process and one
/// that left the batch's process group before starting its own.
#[test]
#[cfg(unix)]
fn python_import_batch_ends_with_its_caller() {
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let pidfile = fixture.root.join("import.pid");
    // Each module writes its pids (and only then is it "running") and hangs.
    let hangs = |setup: &str, pids: &str| {
        format!(
            "import os, subprocess, time\n{setup}\
             open('{0}.tmp', 'w').write({pids})\n\
             os.rename('{0}.tmp', '{0}')\n\
             time.sleep(60)\n",
            pidfile.display()
        )
    };
    let exec_wrapper = python_with_modules(
        &fixture.root,
        &[
            (
                "susi_hangs_with_helper",
                &hangs(
                    "helper = subprocess.Popen(['sleep', '60'])\n",
                    "'%d %d' % (os.getpid(), helper.pid)",
                ),
            ),
            (
                "susi_leaves_the_group",
                &hangs(
                    "os.setpgrp()\nhelper = subprocess.Popen(['sleep', '60'])\n",
                    "'%d %d' % (os.getpid(), helper.pid)",
                ),
            ),
            // The hook's SIGPIPE setting reaches the import, as under `-c`.
            (
                "susi_sees_default_sigpipe",
                &hangs(
                    "import signal\n\
                     assert signal.getsignal(signal.SIGPIPE) == signal.SIG_DFL\n\
                     helper = subprocess.Popen(['sleep', '60'])\n",
                    "'%d %d' % (os.getpid(), helper.pid)",
                ),
            ),
        ],
    );
    let hook = fixture.root.join("sigpipe-hook");
    fs::create_dir_all(&hook).unwrap();
    fs::write(
        hook.join("sitecustomize.py"),
        "import signal\nsignal.signal(signal.SIGPIPE, signal.SIG_DFL)\n",
    )
    .unwrap();
    let sigpipe_wrapper = fixture.root.join("python3-sigpipe");
    write_executable(
        &sigpipe_wrapper,
        &format!(
            "#!/bin/sh\nPYTHONPATH='{}:{}' exec python3 \"$@\"\n",
            hook.display(),
            fixture.root.join("pymods").display()
        ),
    );
    let noexec_wrapper = fixture.root.join("python3-noexec");
    write_executable(
        &noexec_wrapper,
        &format!(
            "#!/bin/sh\nPYTHONPATH='{}' python3 \"$@\"\n",
            fixture.root.join("pymods").display()
        ),
    );
    const CALLER: &str = "import os, subprocess, sys, time
python, script, module, pidfile = sys.argv[1:5]
batch = subprocess.Popen(
    [python, '-c', script, 'token', '30', 'own-group', module],
    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    preexec_fn=os.setpgrp)
print(batch.pid, flush=True)
deadline = time.monotonic() + 20
while not os.path.exists(pidfile) and time.monotonic() < deadline:
    time.sleep(0.05)
os._exit(0)
";
    let wait = std::time::Duration::from_secs(3);
    for (python, module) in [
        (&exec_wrapper, "susi_hangs_with_helper"),
        (&noexec_wrapper, "susi_hangs_with_helper"),
        (&exec_wrapper, "susi_leaves_the_group"),
        (&sigpipe_wrapper, "susi_sees_default_sigpipe"),
    ] {
        let _ = fs::remove_file(&pidfile);
        let out = std::process::Command::new("python3")
            .args(["-c", CALLER])
            .arg(python)
            .arg(catalog::BATCH_SCRIPT)
            .arg(module)
            .arg(&pidfile)
            .output()
            .unwrap();
        let batch = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let case = format!("{module} via {}", python.display());
        let pids = fs::read_to_string(&pidfile)
            .unwrap_or_else(|_| panic!("{case}: the import never started"));
        for pid in std::iter::once(batch.as_str()).chain(pids.split_whitespace()) {
            assert!(
                gone_within(pid, wait),
                "{case}: process {pid} outlived the batch's caller"
            );
        }
    }
}

/// The batch's lifeline is the stdout pipe, never stdin, and its stdin is the
/// null device the single probe has: an interpreter a wrapper hands a null
/// stdin, and one under a startup hook that reads stdin to its end, each still
/// answer every module in one batch and at once.
#[test]
#[cfg(unix)]
fn python_import_batch_does_not_depend_on_stdin() {
    use catalog::ImportVerdict::{Importable, NotImportable};
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let (counting, log) = counting_python(&fixture.root);
    let hook = fixture.root.join("stdin-hook");
    fs::create_dir_all(&hook).unwrap();
    fs::write(
        hook.join("sitecustomize.py"),
        "import sys\nsys.stdin.read()\n",
    )
    .unwrap();
    let null_stdin = fixture.root.join("python3-null-stdin");
    write_executable(
        &null_stdin,
        &format!(
            "#!/bin/sh\nexec '{}' \"$@\" </dev/null\n",
            counting.display()
        ),
    );
    let reads_stdin = fixture.root.join("python3-reads-stdin");
    write_executable(
        &reads_stdin,
        &format!(
            "#!/bin/sh\nPYTHONPATH='{}' exec '{}' \"$@\"\n",
            hook.display(),
            counting.display()
        ),
    );
    let modules = vec!["json".to_string(), "susi_no_such_module_xyz".to_string()];
    for (n, python) in [&null_stdin, &reads_stdin].into_iter().enumerate() {
        let started = std::time::Instant::now();
        let verdicts = catalog::probe_imports(python, &modules, std::time::Duration::from_secs(5));
        let case = python.display();
        assert_eq!(verdicts.get("json"), Some(&Importable), "{case}");
        assert_eq!(
            verdicts.get("susi_no_such_module_xyz"),
            Some(&NotImportable),
            "{case}"
        );
        assert_eq!(starts(&log), n + 1, "{case}: one batch answered both");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{case}: {:?}",
            started.elapsed()
        );
    }
}

/// A stopped susi (Ctrl-Z) does not stop the batch, which runs on in a group
/// of its own and leaves its answers in the pipe. Resumed past its deadline,
/// the caller must read them, not call the import it was waiting on stuck.
/// The test runs `stopped_caller_probe` in a child process, stops that child
/// while its import runs for longer than the caller's bound, and resumes it.
/// Whether the reader thread has passed the answer on by the time the caller
/// looks is a race, so a caller that drops waiting answers fails this test
/// most of the time (7 runs in 10), not every time.
#[test]
#[cfg(unix)]
fn python_import_batch_answers_are_read_after_the_caller_was_stopped() {
    struct Resume(std::process::Child);
    impl Drop for Resume {
        fn drop(&mut self) {
            let pid = self.0.id().to_string();
            let _ = std::process::Command::new("kill")
                .args(["-CONT", &pid])
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::for_kind(CatalogKind::Framework);
    let marker = fixture.root.join("import-started");
    python_with_modules(
        &fixture.root,
        &[(
            "susi_slow_import",
            &format!(
                "import time\nopen('{}', 'w').close()\ntime.sleep(0.5)\n",
                marker.display()
            ),
        )],
    );
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "external::tests::stopped_caller_probe",
            "--test-threads=1",
        ])
        .env("SUSI_STOPPED_CALLER_ROOT", &fixture.root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut child = Resume(child);
    let pid = child.0.id().to_string();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < until,
            "the import never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Stopped past the caller's bound (per-import 1 s + 2 s); the batch
    // answers within its own 1 s meanwhile.
    assert!(std::process::Command::new("kill")
        .args(["-STOP", &pid])
        .status()
        .unwrap()
        .success());
    std::thread::sleep(std::time::Duration::from_secs(4));
    assert!(std::process::Command::new("kill")
        .args(["-CONT", &pid])
        .status()
        .unwrap()
        .success());
    assert!(
        child.0.wait().unwrap().success(),
        "the probe in the child failed"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("verdicts")).unwrap(),
        "Some(Importable) Some(Importable)",
        "an answer that waited in the pipe while the caller was stopped was lost"
    );
}

/// The child half of the test above; without its fixture it does nothing.
#[test]
#[ignore = "run by python_import_batch_answers_are_read_after_the_caller_was_stopped"]
#[cfg(unix)]
fn stopped_caller_probe() {
    let Some(root) = std::env::var_os("SUSI_STOPPED_CALLER_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let verdicts = catalog::probe_imports(
        &root.join("python3-with-modules"),
        &["susi_slow_import".to_string(), "json".to_string()],
        std::time::Duration::from_secs(1),
    );
    fs::write(
        root.join("verdicts"),
        format!(
            "{:?} {:?}",
            verdicts.get("susi_slow_import"),
            verdicts.get("json")
        ),
    )
    .unwrap();
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

/// A Python interpreter that runs, resolved as a preflight resolves one (a
/// Windows host's `python3` may be the App Installer alias, which only
/// prints that Python is not installed). `None` on a host without one: the
/// tests that need a real interpreter say so and skip.
fn working_python() -> Option<PathBuf> {
    let found = ["python3", "python"]
        .iter()
        .filter_map(|name| catalog::resolve_program(name))
        .find(|python| {
            std::process::Command::new(python)
                .args(["-c", "import sys"])
                .output()
                .is_ok_and(|out| out.status.success())
        });
    if found.is_none() {
        eprintln!("skipped: no working Python interpreter on PATH");
    }
    found
}

/// Each module's own probe keeps only an answer the single probe run alone
/// would give: the import succeeded, or the interpreter said the module is
/// not installed. An import cut off at its bound, an interpreter cut down
/// mid-run, one that cannot start, or an import that failed any other way
/// (an installed module missing a dependency, or short of memory beside its
/// neighbours) decides nothing, so the preflight runs that module's single
/// probe by itself, as it always did.
#[test]
fn per_module_import_probes_keep_only_the_single_probes_answers() {
    use catalog::ImportVerdict::{Importable, NotImportable};
    let Some(python) = working_python() else {
        return;
    };
    // The probe runs `import <module>`: statements after the name stand in
    // for an import that never finishes, one whose interpreter is cut down,
    // and one that exits with a failure of its own.
    let hangs = "json; import time; time.sleep(30)".to_string();
    let cut_down = if cfg!(windows) {
        // STATUS_DLL_INIT_FAILED, as a busy Windows host ends a process.
        "json; import os; os._exit(-1073741502)"
    } else {
        "json; import os, signal; os.kill(os.getpid(), signal.SIGKILL)"
    }
    .to_string();
    let fails = "json; raise SystemExit(3)".to_string();
    // An installed module whose own import misses a dependency.
    let broken = "json; import susi_missing_dependency_xyz".to_string();
    let modules = vec![
        "json".to_string(),
        "os.path".to_string(),
        "susi_no_such_module_xyz".to_string(),
        "os.susi_no_such_submodule_xyz".to_string(),
        hangs.clone(),
        cut_down.clone(),
        fails.clone(),
        broken.clone(),
    ];
    let started = std::time::Instant::now();
    let verdicts =
        catalog::probe_imports_each(&python, &modules, std::time::Duration::from_secs(3));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(28),
        "the hanging import was cut off at its bound"
    );
    assert_eq!(verdicts.get("json"), Some(&Importable));
    assert_eq!(verdicts.get("os.path"), Some(&Importable));
    assert_eq!(
        verdicts.get("susi_no_such_module_xyz"),
        Some(&NotImportable)
    );
    assert_eq!(
        verdicts.get("os.susi_no_such_submodule_xyz"),
        Some(&NotImportable)
    );
    assert_eq!(
        verdicts.get(&fails),
        None,
        "a failure of its own: left to the single probe"
    );
    assert_eq!(
        verdicts.get(&broken),
        None,
        "a missing dependency: left to the single probe"
    );
    assert_eq!(
        verdicts.get(&hangs),
        None,
        "cut off: left to the single probe"
    );
    assert_eq!(
        verdicts.get(&cut_down),
        None,
        "cut down: left to the single probe"
    );
    assert!(
        catalog::probe_imports_each(
            Path::new("/no/such/python"),
            &modules[..1],
            std::time::Duration::from_secs(3)
        )
        .is_empty(),
        "an interpreter that cannot start decides nothing: the single probe reports it"
    );
}

/// A scan's preflights reach exactly the verdicts the single probe reaches
/// outside a scan, whichever way the scan probed (one batch interpreter
/// where `fork` exists, one interpreter per module elsewhere).
#[test]
fn a_scans_import_verdicts_are_the_single_probes() {
    let Some(python) = working_python() else {
        return;
    };
    let config =
        std::env::temp_dir().join(format!("susi-pyimport-parity-{}.json", std::process::id()));
    fs::write(&config, "{}").unwrap();
    std::env::set_var("SUSI_PYIMPORT_PARITY_CONFIG", &config);
    let adapter = |module: &str| Adapter::Python {
        python: python.to_string_lossy().into_owned(),
        import_name: module.into(),
        config_env: "SUSI_PYIMPORT_PARITY_CONFIG".into(),
    };
    let adapters = vec![
        adapter("json"),
        adapter("susi_no_such_module_xyz"),
        adapter("os.path"),
        adapter("json; raise SystemExit(3)"),
        adapter("json; import susi_missing_dependency_xyz"),
    ];
    let preflights = || {
        adapters
            .iter()
            .map(|a| a.preflight().map(|_| ()).map_err(|e| e.to_string()))
            .collect::<Vec<_>>()
    };
    let in_scan = catalog::with_python_imports(&adapters, preflights);
    let alone = preflights();
    std::env::remove_var("SUSI_PYIMPORT_PARITY_CONFIG");
    let _ = fs::remove_file(&config);
    assert_eq!(in_scan, alone);
    assert!(in_scan[0].is_ok() && in_scan[2].is_ok(), "{in_scan:?}");
    assert!(
        in_scan[1].is_err() && in_scan[3].is_err() && in_scan[4].is_err(),
        "{in_scan:?}"
    );
}

/// Without `fork` there is no batch interpreter: a scan's imports are each
/// probed in their own interpreter, several side by side, so the scan waits
/// for about one import's time rather than the sum of them. Unix forks one
/// batch interpreter instead
/// (`python_imports_are_probed_in_one_interpreter_per_scan`). An import
/// still running at [`catalog::SIDE_BY_SIDE_BOUND`] is cut there and left to
/// its single probe, which gives it the whole per-import bound on its own:
/// it costs the scan that short bound, never a second full one.
#[test]
#[cfg(not(unix))]
fn without_fork_a_scans_imports_are_probed_side_by_side() {
    use catalog::ImportVerdict::Importable;
    let Some(python) = working_python() else {
        return;
    };
    let bound = std::time::Duration::from_secs(30);
    // Each import takes a second; one after another they take eight, and
    // more for each interpreter start.
    let modules: Vec<String> = (0..8)
        .map(|i| format!("json; import time; time.sleep(1); x = {i}"))
        .collect();
    let alone = std::time::Instant::now();
    assert_eq!(
        catalog::probe_imports(&python, &modules[..1], bound).get(&modules[0]),
        Some(&Importable)
    );
    let one = alone.elapsed();
    let started = std::time::Instant::now();
    let verdicts = catalog::probe_imports(&python, &modules, bound);
    let elapsed = started.elapsed();
    for module in &modules {
        assert_eq!(verdicts.get(module), Some(&Importable), "{module}");
    }
    if std::thread::available_parallelism().map_or(1, |n| n.get()) >= 4 {
        assert!(
            elapsed < one * 6,
            "the probes overlapped: {elapsed:?} for eight imports of {one:?} each"
        );
    }

    // Twenty seconds would finish within the per-import bound; side by side
    // it is cut at the short one and left to the single probe.
    let slow = "json; import time; time.sleep(20)".to_string();
    let started = std::time::Instant::now();
    let verdicts = catalog::probe_imports(&python, std::slice::from_ref(&slow), bound);
    assert_eq!(verdicts.get(&slow), None, "left to its single probe");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(18),
        "cut at the side-by-side bound: {:?}",
        started.elapsed()
    );
}
