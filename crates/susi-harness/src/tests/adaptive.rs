use super::*;

fn config(model: &str) -> ExecutorConfig {
    ExecutorConfig {
        gateway_url: "http://127.0.0.1:1".into(),
        model: model.into(),
        workspace: std::env::temp_dir(),
        state_dir: std::env::temp_dir().join("susi-adaptive-test-state"),
        mission: "adaptive".into(),
        task: "route-1".into(),
        attempt: 1,
        timeout: Duration::from_secs(30),
        prompt: "say OK".into(),
    }
}

/// A runner that records the model of every turn it is handed and answers
/// with a fixed outcome.
struct Scripted {
    outcome: RunOutcome,
    seen: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(outcome: RunOutcome) -> Self {
        Self {
            outcome,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn ok(text: &str) -> Self {
        Self::new(RunOutcome::Succeeded {
            final_text: text.into(),
            reported_cost_usd: None,
        })
    }

    fn failed(message: &str) -> Self {
        Self::new(RunOutcome::Failed {
            message: message.into(),
        })
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Runner for Scripted {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        self.seen.lock().unwrap().push(config.model.clone());
        Ok(self.outcome.clone())
    }
}

fn text(outcome: &RunOutcome) -> &str {
    match outcome {
        RunOutcome::Succeeded { final_text, .. } => final_text,
        other => panic!("expected success, got {other:?}"),
    }
}

fn system32(program: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(program)
}

#[test]
fn strategies_parse_from_their_env_names_and_default_to_auto() {
    assert_eq!(
        AdaptiveStrategy::parse("auto"),
        Some(AdaptiveStrategy::Auto)
    );
    assert_eq!(
        AdaptiveStrategy::parse(" CLI-first "),
        Some(AdaptiveStrategy::PreferCliSubThenPayg)
    );
    assert_eq!(
        AdaptiveStrategy::parse("payg_first"),
        Some(AdaptiveStrategy::PreferPaygThenCliSub)
    );
    assert_eq!(
        AdaptiveStrategy::parse("payg-only"),
        Some(AdaptiveStrategy::PaygOnly)
    );
    assert_eq!(
        AdaptiveStrategy::parse("cli-only"),
        Some(AdaptiveStrategy::CliSubOnly)
    );
    assert_eq!(AdaptiveStrategy::parse("sideways"), None);
    assert_eq!(AdaptiveStrategy::default(), AdaptiveStrategy::Auto);
    for strategy in [
        AdaptiveStrategy::Auto,
        AdaptiveStrategy::PreferCliSubThenPayg,
        AdaptiveStrategy::PreferPaygThenCliSub,
        AdaptiveStrategy::PaygOnly,
        AdaptiveStrategy::CliSubOnly,
    ] {
        assert_eq!(AdaptiveStrategy::parse(strategy.as_str()), Some(strategy));
    }
}

#[test]
fn cli_specs_parse_with_prefix_tool_and_optional_submodel() {
    let cases = [
        ("cli:claude", CliToolKind::Claude, None),
        ("cli:claude:opus", CliToolKind::Claude, Some("opus")),
        ("sub:codex", CliToolKind::Codex, None),
        ("cli:gemini", CliToolKind::Gemini, None),
        ("cli:agy", CliToolKind::Antigravity, None),
        ("cli:qwen", CliToolKind::Qwen, None),
        ("cli:aider", CliToolKind::Aider, None),
        ("cli:opencode", CliToolKind::OpenCode, None),
        (
            "SUB:Gemini:gemini-2.5-pro",
            CliToolKind::Gemini,
            Some("gemini-2.5-pro"),
        ),
    ];
    for (spec, kind, submodel) in cases {
        assert_eq!(
            CliToolKind::parse_spec(spec),
            Some((kind, submodel.map(str::to_string))),
            "{spec}"
        );
    }
    for not_cli in ["deepseek-flash", "openai:gpt-5", "cli:", "cli:vim", ""] {
        assert_eq!(CliToolKind::parse_spec(not_cli), None, "{not_cli}");
    }
    assert_eq!(
        CliToolKind::parse_spec("cli:claude:"),
        Some((CliToolKind::Claude, None))
    );
    for kind in CliToolKind::ALL {
        assert_eq!(CliToolKind::from_name(kind.as_str()), Some(kind));
    }
}

#[test]
fn invocations_pipe_the_prompt_on_stdin_for_every_tool() {
    let strings = |args: &[&str]| args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();

    let (programs, args, pipe) = build_invocation(CliToolKind::Claude, None);
    assert_eq!(programs, ["claude", "claude.exe"]);
    assert_eq!(
        args,
        strings(&["--dangerously-skip-permissions", "-p", STDIN_PROMPT])
    );
    assert!(pipe);
    let (_, args, _) = build_invocation(CliToolKind::Claude, Some("opus"));
    assert_eq!(
        args,
        strings(&[
            "--dangerously-skip-permissions",
            "-p",
            STDIN_PROMPT,
            "--model",
            "opus"
        ])
    );

    let (programs, args, _) = build_invocation(CliToolKind::Codex, None);
    assert_eq!(programs, ["codex", "codex.exe"]);
    assert_eq!(
        args,
        strings(&[
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
            "-"
        ])
    );
    let (_, args, _) = build_invocation(CliToolKind::Codex, Some("o3"));
    assert_eq!(
        args,
        strings(&[
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
            "-",
            "-m",
            "o3"
        ])
    );

    let (programs, args, _) = build_invocation(CliToolKind::Gemini, Some("flash"));
    assert_eq!(programs, ["gemini", "gemini.cmd", "gemini.exe"]);
    assert_eq!(
        args,
        strings(&["--yolo", "-p", STDIN_PROMPT, "-m", "flash"])
    );

    let (programs, args, _) =
        build_invocation(CliToolKind::Antigravity, Some("gemini-4-argon-high"));
    assert_eq!(programs, ["agy", "agy.exe", "antigravity"]);
    assert_eq!(
        args,
        strings(&[
            "--sandbox",
            "--print-timeout",
            "0",
            "--print",
            STDIN_PROMPT,
            "--model",
            "gemini-4-argon-high"
        ])
    );

    let (programs, args, _) = build_invocation(CliToolKind::Qwen, None);
    assert_eq!(programs, ["qwen", "qwen.cmd", "qwen.exe"]);
    assert_eq!(args, strings(&["--yolo", "-p", STDIN_PROMPT]));

    let (programs, args, _) = build_invocation(CliToolKind::Aider, None);
    assert_eq!(programs, ["aider", "aider.exe"]);
    assert_eq!(
        args,
        strings(&["--yes-always", "--no-auto-commits", "--message-file", "-"])
    );

    let (programs, args, _) = build_invocation(CliToolKind::OpenCode, None);
    assert_eq!(programs, ["opencode", "opencode.exe"]);
    assert_eq!(args, strings(&["run"]));

    for kind in CliToolKind::ALL {
        assert!(build_invocation(kind, None).2, "{kind:?} must pipe stdin");
    }
}

#[test]
fn discovery_finds_mock_binaries_and_the_first_directory_wins() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("claude.exe"), b"").unwrap();
    std::fs::write(first.path().join("gemini.cmd"), b"").unwrap();
    std::fs::write(second.path().join("claude"), b"").unwrap();
    std::fs::write(second.path().join("agy"), b"").unwrap();
    // A directory named like a tool is not a tool.
    std::fs::create_dir(second.path().join("codex")).unwrap();

    let dirs = vec![first.path().to_path_buf(), second.path().to_path_buf()];
    let found = discover_cli_tools_in(&dirs);
    let kinds: Vec<CliToolKind> = found.iter().map(|tool| tool.kind).collect();
    assert_eq!(
        kinds,
        [
            CliToolKind::Claude,
            CliToolKind::Gemini,
            CliToolKind::Antigravity
        ]
    );
    assert_eq!(found[0].path, first.path().join("claude.exe"));
    assert_eq!(found[1].path, first.path().join("gemini.cmd"));
    assert_eq!(found[2].path, second.path().join("agy"));
    assert!(discover_cli_tools_in(&[]).is_empty());
    assert!(!default_search_dirs().is_empty());
    // The live scan never panics, whatever this machine has installed.
    let _ = discover_cli_tools();
}

#[test]
fn antigravity_resolves_the_requested_model_without_permission_bypass() {
    let installed = tempfile::tempdir().unwrap();
    std::fs::write(installed.path().join("agy.exe"), b"").unwrap();
    let runner = CliSubRunner::default().with_search_dirs(vec![installed.path().to_path_buf()]);
    let resolved = runner
        .resolve("cli:antigravity:gemini-4-argon-high")
        .unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].program, installed.path().join("agy.exe"));
    assert!(resolved[0]
        .args
        .windows(2)
        .any(|pair| pair == ["--model", "gemini-4-argon-high"]));
    assert!(resolved[0]
        .args
        .windows(2)
        .any(|pair| pair == ["--print-timeout", "0"]));
    assert!(resolved[0].args.iter().any(|arg| arg == "--sandbox"));
    assert!(!resolved[0]
        .args
        .iter()
        .any(|arg| arg == "--yolo" || arg == "--dangerously-skip-permissions"));
    // The unspecified case does not invent or override the user's model.
    let (_, args, _) = build_invocation(CliToolKind::Antigravity, None);
    assert!(!args.iter().any(|arg| arg == "--model"));
}

// Scripted runners test outer routing only; real CLI enforcement is tested
// separately through the production builder below.
impl AdaptiveRunner<Scripted, Scripted> {
    fn with_scripted_argon_policy(mut self) -> Self {
        self.antigravity_argon_only = true;
        self
    }
}

#[test]
fn argon_builder_enforces_cli_policy_before_an_override_can_launch() {
    let runner = AdaptiveRunner::new(
        Scripted::ok("PAYG must not run"),
        CliSubRunner::with_command(sort_command()),
        AdaptiveStrategy::PreferCliSubThenPayg,
    )
    .with_fallback_payg_model("paid-model")
    .with_antigravity_argon_only();
    assert!(runner.antigravity_argon_only);
    assert!(runner.cli_runner.antigravity_argon_only);
    // sort would succeed if spawned, so only this exact refusal proves the
    // builder propagated policy into CLI resolution before process launch.
    let outcome = runner.run(&config("cli:agy")).unwrap();
    assert!(matches!(outcome, RunOutcome::Failed { ref message }
        if message.contains("refuses an unverified command override")));
    assert!(runner.payg_runner.seen().is_empty());
}

#[test]
fn argon_policy_pins_missing_model_and_refuses_conflicts_before_launch() {
    let installed = tempfile::tempdir().unwrap();
    std::fs::write(installed.path().join("agy.exe"), b"").unwrap();
    let runner = CliSubRunner {
        antigravity_argon_only: true,
        ..CliSubRunner::default().with_search_dirs(vec![installed.path().to_path_buf()])
    };
    for spec in [
        "cli:agy",
        "sub:antigravity",
        "cli:antigravity:gemini-4-argon-high",
    ] {
        let command = runner.resolve(spec).unwrap();
        assert!(command[0]
            .args
            .windows(2)
            .any(|pair| pair == ["--model", ANTIGRAVITY_ARGON_MODEL]));
    }
    for spec in ["cli:agy:other", "sub:antigravity:gemini-4-argon-low"] {
        assert!(runner.resolve(spec).unwrap_err().contains("Argon-only"));
    }
    let overridden = CliSubRunner {
        antigravity_argon_only: true,
        ..CliSubRunner::with_command(sort_command())
    };
    assert!(overridden.resolve("cli:agy").is_err());
}

#[test]
fn argon_policy_never_spends_payg_for_an_antigravity_request() {
    for strategy in [
        AdaptiveStrategy::Auto,
        AdaptiveStrategy::PreferCliSubThenPayg,
        AdaptiveStrategy::PreferPaygThenCliSub,
        AdaptiveStrategy::PaygOnly,
        AdaptiveStrategy::CliSubOnly,
    ] {
        for outcome in [
            RunOutcome::Failed {
                message: "quota exhausted".into(),
            },
            RunOutcome::TimedOut,
        ] {
            let runner = AdaptiveRunner::new(
                Scripted::ok("must not run"),
                Scripted::new(outcome.clone()),
                strategy,
            )
            .with_fallback_payg_model("paid-model")
            .with_scripted_argon_policy();
            assert!(runner.plan("cli:agy").failover.is_none());
            assert_eq!(runner.run(&config("cli:agy")).unwrap(), outcome);
            assert!(runner.payg_runner.seen().is_empty());
            assert_eq!(
                runner.cli_runner.seen(),
                [format!("cli:antigravity:{ANTIGRAVITY_ARGON_MODEL}")]
            );
        }
        let runner = AdaptiveRunner::new(
            Scripted::ok("must not run"),
            Scripted::ok("must not run"),
            strategy,
        )
        .with_fallback_payg_model("paid-model")
        .with_scripted_argon_policy();
        assert!(matches!(
            runner.run(&config("cli:agy:other")).unwrap(),
            RunOutcome::Failed { .. }
        ));
        assert!(runner.payg_runner.seen().is_empty());
        assert!(runner.cli_runner.seen().is_empty());
    }
}

#[test]
fn argon_policy_from_env_wires_both_production_layers() {
    let runner = AdaptiveRunner::from_env();
    let expected = std::env::var_os(ANTIGRAVITY_ARGON_ONLY_ENV).is_some();
    assert_eq!(runner.antigravity_argon_only, expected);
    assert_eq!(runner.cli_runner.antigravity_argon_only, expected);
    if expected {
        assert!(runner.plan("cli:antigravity").failover.is_none());
        assert!(matches!(
            runner.run(&config("cli:agy:refused-model")).unwrap(),
            RunOutcome::Failed { .. }
        ));
    }
}

#[test]
fn argon_policy_is_opt_in_and_does_not_restrict_other_routes() {
    let unrestricted = AdaptiveRunner::new(
        Scripted::ok("payg"),
        Scripted::ok("cli"),
        AdaptiveStrategy::Auto,
    );
    unrestricted.run(&config("cli:agy:other")).unwrap();
    assert_eq!(unrestricted.cli_runner.seen(), ["cli:agy:other"]);
    let restricted = AdaptiveRunner::new(
        Scripted::ok("payg"),
        Scripted::ok("cli"),
        AdaptiveStrategy::Auto,
    )
    .with_scripted_argon_policy();
    restricted.run(&config("cli:claude:opus")).unwrap();
    restricted.run(&config("paid-model")).unwrap();
    assert_eq!(restricted.cli_runner.seen(), ["cli:claude:opus"]);
    assert_eq!(restricted.payg_runner.seen(), ["paid-model"]);
}

#[test]
fn default_discovery_includes_antigravity_local_installation() {
    // Read-only environment inspection avoids racing parallel tests.
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let expected = PathBuf::from(local).join("agy").join("bin");
        assert!(default_search_dirs().contains(&expected));
    }
}

#[test]
fn quota_and_credit_classifiers_recognise_provider_refusals() {
    for quota in [
        "Claude AI usage limit reached|1730000000",
        "You have hit your limit - resets at 5pm",
        "429 Too Many Requests",
        "Rate limit exceeded for this session",
        "Quota exceeded for quota metric",
    ] {
        assert!(is_subscription_quota_exhausted(quota), "{quota}");
    }
    assert!(!is_subscription_quota_exhausted("compiled 12 crates"));

    for credit in [
        "{\"error\":{\"code\":\"insufficient_quota\"}}",
        "Insufficient Balance",
        "HTTP 402 Payment Required",
        "401 Unauthorized: invalid API key",
        "none of the configured providers accepted the request",
        "check your billing details",
        "no credit remaining",
    ] {
        assert!(is_payg_credit_or_key_exhausted(credit), "{credit}");
    }
    assert!(!is_payg_credit_or_key_exhausted("all tests passed"));
}

#[test]
fn payg_only_and_cli_only_never_touch_the_other_plane() {
    let runner = AdaptiveRunner::new(
        Scripted::failed("insufficient_quota"),
        Scripted::ok("cli"),
        AdaptiveStrategy::PaygOnly,
    )
    .with_fallback_cli_model("cli:claude");
    let outcome = runner.run(&config("deepseek-flash")).unwrap();
    assert!(matches!(outcome, RunOutcome::Failed { .. }));
    assert!(runner.cli_runner.seen().is_empty());

    let runner = AdaptiveRunner::new(
        Scripted::ok("payg"),
        Scripted::failed("usage limit reached"),
        AdaptiveStrategy::CliSubOnly,
    )
    .with_fallback_payg_model("deepseek-flash");
    let outcome = runner.run(&config("cli:claude")).unwrap();
    assert!(matches!(outcome, RunOutcome::Failed { .. }));
    assert!(runner.payg_runner.seen().is_empty());
    assert_eq!(runner.cli_runner.seen(), ["cli:claude"]);
}

#[test]
fn cli_first_fails_over_to_payg_on_quota_exhaustion_with_the_fallback_model() {
    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::failed("Claude AI usage limit reached; resets at 5pm"),
        AdaptiveStrategy::PreferCliSubThenPayg,
    )
    .with_fallback_payg_model("deepseek-flash");
    let outcome = runner.run(&config("cli:claude:opus")).unwrap();
    assert_eq!(text(&outcome), "from payg");
    assert_eq!(runner.cli_runner.seen(), ["cli:claude:opus"]);
    assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
}

#[test]
fn cli_first_fails_over_on_timeout_and_keeps_a_payg_model_as_is() {
    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::new(RunOutcome::TimedOut),
        AdaptiveStrategy::PreferCliSubThenPayg,
    );
    let outcome = runner.run(&config("glm-4.6")).unwrap();
    assert_eq!(text(&outcome), "from payg");
    assert_eq!(runner.payg_runner.seen(), ["glm-4.6"]);

    // A cli: spec with no PAYG fallback has nowhere to go.
    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::new(RunOutcome::TimedOut),
        AdaptiveStrategy::PreferCliSubThenPayg,
    );
    assert_eq!(
        runner.run(&config("cli:codex")).unwrap(),
        RunOutcome::TimedOut
    );
    assert!(runner.payg_runner.seen().is_empty());
}

#[test]
fn cli_first_success_never_spends_payg_money() {
    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::ok("from cli"),
        AdaptiveStrategy::PreferCliSubThenPayg,
    )
    .with_fallback_payg_model("deepseek-flash");
    assert_eq!(
        text(&runner.run(&config("cli:claude")).unwrap()),
        "from cli"
    );
    assert!(runner.payg_runner.seen().is_empty());
}

#[test]
fn payg_first_fails_over_to_the_cli_on_a_credit_refusal() {
    let runner = AdaptiveRunner::new(
        Scripted::failed("HTTP 402: insufficient balance"),
        Scripted::ok("from cli"),
        AdaptiveStrategy::PreferPaygThenCliSub,
    )
    .with_fallback_cli_model("cli:codex");
    let outcome = runner.run(&config("deepseek-flash")).unwrap();
    assert_eq!(text(&outcome), "from cli");
    assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
    assert_eq!(runner.cli_runner.seen(), ["cli:codex"]);
}

#[test]
fn payg_first_does_not_fail_over_on_a_payg_timeout() {
    let runner = AdaptiveRunner::new(
        Scripted::new(RunOutcome::TimedOut),
        Scripted::ok("from cli"),
        AdaptiveStrategy::PreferPaygThenCliSub,
    )
    .with_fallback_cli_model("cli:codex");
    assert_eq!(
        runner.run(&config("deepseek-flash")).unwrap(),
        RunOutcome::TimedOut
    );
    assert!(runner.cli_runner.seen().is_empty());
}

#[test]
fn payg_first_with_a_cli_spec_runs_payg_only_through_the_fallback_model() {
    let runner = AdaptiveRunner::new(
        Scripted::failed("401 invalid api key"),
        Scripted::ok("from cli"),
        AdaptiveStrategy::PreferPaygThenCliSub,
    )
    .with_fallback_payg_model("deepseek-flash");
    assert_eq!(
        text(&runner.run(&config("cli:gemini")).unwrap()),
        "from cli"
    );
    assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
    assert_eq!(runner.cli_runner.seen(), ["cli:gemini"]);

    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::ok("from cli"),
        AdaptiveStrategy::PreferPaygThenCliSub,
    );
    assert_eq!(
        text(&runner.run(&config("cli:gemini")).unwrap()),
        "from cli"
    );
    assert!(runner.payg_runner.seen().is_empty());
}

#[test]
fn auto_routes_by_model_spec_and_fails_over_only_when_configured() {
    // A cli: spec runs on the CLI and fails over to the PAYG fallback.
    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::failed("You have hit your limit"),
        AdaptiveStrategy::Auto,
    )
    .with_fallback_payg_model("deepseek-flash");
    assert_eq!(
        text(&runner.run(&config("cli:claude")).unwrap()),
        "from payg"
    );
    assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);

    // Without the fallback the CLI verdict stands unchanged.
    let runner = AdaptiveRunner::new(
        Scripted::ok("from payg"),
        Scripted::failed("You have hit your limit"),
        AdaptiveStrategy::Auto,
    );
    assert_eq!(
        runner.run(&config("cli:claude")).unwrap(),
        RunOutcome::Failed {
            message: "You have hit your limit".into()
        }
    );
    assert!(runner.payg_runner.seen().is_empty());

    // A PAYG id runs on PAYG and fails over to the CLI fallback.
    let runner = AdaptiveRunner::new(
        Scripted::failed("insufficient_quota"),
        Scripted::ok("from cli"),
        AdaptiveStrategy::Auto,
    )
    .with_fallback_cli_model("cli:claude:sonnet");
    assert_eq!(
        text(&runner.run(&config("deepseek-flash")).unwrap()),
        "from cli"
    );
    assert_eq!(runner.cli_runner.seen(), ["cli:claude:sonnet"]);

    // Without the fallback a PAYG id never shells out.
    let runner = AdaptiveRunner::new(
        Scripted::failed("insufficient_quota"),
        Scripted::ok("from cli"),
        AdaptiveStrategy::Auto,
    );
    assert!(matches!(
        runner.run(&config("deepseek-flash")).unwrap(),
        RunOutcome::Failed { .. }
    ));
    assert!(runner.cli_runner.seen().is_empty());
}

#[test]
fn a_double_failure_reports_both_planes() {
    let runner = AdaptiveRunner::new(
        Scripted::failed("insufficient_quota"),
        Scripted::failed("usage limit reached"),
        AdaptiveStrategy::PreferPaygThenCliSub,
    )
    .with_fallback_cli_model("cli:claude");
    let RunOutcome::Failed { message } = runner.run(&config("deepseek-flash")).unwrap() else {
        panic!("expected failure");
    };
    assert!(
        message.contains("payg-byok (deepseek-flash) failed: insufficient_quota"),
        "{message}"
    );
    assert!(
        message.contains("monthly-sub-cli (cli:claude) failed: usage limit reached"),
        "{message}"
    );
}

#[test]
fn a_missing_cli_tool_is_a_failed_turn_that_fails_over_to_payg() {
    let empty = tempfile::tempdir().unwrap();
    let cli = CliSubRunner::default().with_search_dirs(vec![empty.path().to_path_buf()]);
    let outcome = cli.run(&config("cli:claude")).unwrap();
    let RunOutcome::Failed { message } = &outcome else {
        panic!("expected failure, got {outcome:?}");
    };
    assert!(message.contains("claude"), "{message}");
    assert!(matches!(
        cli.run(&config("deepseek-flash")).unwrap(),
        RunOutcome::Failed { .. }
    ));

    let runner = AdaptiveRunner::new(Scripted::ok("from payg"), cli, AdaptiveStrategy::Auto)
        .with_fallback_payg_model("deepseek-flash");
    assert_eq!(
        text(&runner.run(&config("cli:claude")).unwrap()),
        "from payg"
    );
    assert_eq!(runner.payg_runner.seen(), ["deepseek-flash"]);
}

#[test]
fn cli_resolution_prefers_the_spec_then_the_default_then_discovery() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("codex"), b"").unwrap();
    std::fs::write(dir.path().join("claude"), b"").unwrap();
    let dirs = vec![dir.path().to_path_buf()];

    let discovered = CliSubRunner::default().with_search_dirs(dirs.clone());
    let commands = discovered.resolve("deepseek-flash").unwrap();
    assert_eq!(commands[0].program, dir.path().join("claude"));

    let defaulted =
        CliSubRunner::with_default_kind(CliToolKind::Codex).with_search_dirs(dirs.clone());
    let commands = defaulted.resolve("deepseek-flash").unwrap();
    assert_eq!(commands[0].program, dir.path().join("codex"));
    assert_eq!(
        commands[0].args,
        [
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "--skip-git-repo-check",
            "-"
        ]
    );

    let commands = defaulted.resolve("cli:claude:opus").unwrap();
    assert_eq!(commands[0].program, dir.path().join("claude"));
    assert_eq!(commands[0].args.last().map(String::as_str), Some("opus"));

    // With no explicit search list an absent tool falls back to PATH
    // lookup over every candidate name.
    let path_lookup = CliSubRunner::with_default_kind(CliToolKind::Gemini);
    if find_in_dirs(CliToolKind::Gemini, &default_search_dirs()).is_none() {
        let programs: Vec<PathBuf> = path_lookup
            .resolve("x")
            .unwrap()
            .into_iter()
            .map(|command| command.program)
            .collect();
        assert_eq!(
            programs,
            ["gemini", "gemini.cmd", "gemini.exe"].map(PathBuf::from)
        );
    }
}

/// A command on every platform that copies stdin to stdout (sorted).
fn sort_command() -> CliCommand {
    let program = if cfg!(windows) {
        system32("sort.exe")
    } else {
        PathBuf::from("sort")
    };
    CliCommand {
        program,
        args: Vec::new(),
        pipe_prompt_on_stdin: true,
    }
}

/// A command on every platform that runs for about 30 seconds.
fn slow_command() -> (PathBuf, Vec<String>) {
    if cfg!(windows) {
        (
            system32("PING.EXE"),
            vec!["-n".into(), "30".into(), "127.0.0.1".into()],
        )
    } else {
        (PathBuf::from("sleep"), vec!["30".into()])
    }
}

#[test]
fn cli_runner_pipes_a_multi_line_quoted_prompt_on_stdin_at_zero_payg_cost() {
    let prompt = "line one says \"hello world\"\n\
                  line two has 'single' quotes and a \\backslash\n\
                  line three has %PATH% $HOME & | < > ^ chars\n";
    let runner = CliSubRunner::with_command(sort_command());
    let mut cfg = config("cli:claude");
    cfg.prompt = prompt.to_string();
    let outcome = runner.run(&cfg).unwrap();
    let RunOutcome::Succeeded {
        final_text,
        reported_cost_usd,
    } = outcome
    else {
        panic!("expected success, got {outcome:?}");
    };
    assert_eq!(reported_cost_usd, Some(0.0));
    for line in prompt.lines() {
        assert!(
            final_text.lines().any(|out| out.trim_end() == line),
            "missing {line:?} in {final_text:?}"
        );
    }
}

#[test]
fn cli_runner_drains_output_larger_than_a_pipe_buffer() {
    let prompt: String = (0..6_000)
        .map(|n| format!("row {n:05} padding padding padding\n"))
        .collect();
    assert!(prompt.len() > 128 * 1024);
    let command = sort_command();
    let outcome = run_piped(
        &command,
        &std::env::temp_dir(),
        &prompt,
        Duration::from_secs(60),
        DEFAULT_POLL_INTERVAL,
    );
    let PipedRun::Finished(RunOutcome::Succeeded { final_text, .. }) = outcome else {
        panic!("expected success, got {outcome:?}");
    };
    assert_eq!(final_text.lines().count(), 6_000);
}

#[test]
fn cli_runner_kills_a_child_that_outlives_its_timeout() {
    let (program, args) = slow_command();
    let runner = CliSubRunner::with_command(CliCommand {
        program,
        args,
        pipe_prompt_on_stdin: true,
    });
    let mut cfg = config("cli:claude");
    cfg.timeout = Duration::from_millis(400);
    let started = Instant::now();
    assert_eq!(runner.run(&cfg).unwrap(), RunOutcome::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "timeout took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_non_zero_exit_is_a_failed_turn_quoting_the_output() {
    let (program, args): (PathBuf, Vec<String>) = if cfg!(windows) {
        (
            system32("cmd.exe"),
            vec![
                "/D".into(),
                "/C".into(),
                "echo Claude usage limit reached 1>&2 & exit 3".into(),
            ],
        )
    } else {
        (
            PathBuf::from("sh"),
            vec![
                "-c".into(),
                "echo Claude usage limit reached 1>&2; exit 3".into(),
            ],
        )
    };
    let runner = CliSubRunner::with_command(CliCommand {
        program,
        args,
        pipe_prompt_on_stdin: false,
    });
    let RunOutcome::Failed { message } = runner.run(&config("cli:claude")).unwrap() else {
        panic!("expected failure");
    };
    assert!(message.contains("code 3"), "{message}");
    assert!(message.contains("usage limit reached"), "{message}");
    assert!(
        message.contains("[subscription quota exhausted]"),
        "{message}"
    );
}

#[test]
fn output_survives_a_grandchild_that_holds_the_pipe_open() {
    let (program, args): (PathBuf, Vec<String>) = if cfg!(windows) {
        (
            system32("cmd.exe"),
            vec![
                "/D".into(),
                "/C".into(),
                "start /B PING -n 8 127.0.0.1 >NUL & echo visible".into(),
            ],
        )
    } else {
        (
            PathBuf::from("sh"),
            vec!["-c".into(), "sleep 8 & echo visible".into()],
        )
    };
    let runner = CliSubRunner::with_command(CliCommand {
        program,
        args,
        pipe_prompt_on_stdin: false,
    });
    let started = Instant::now();
    let outcome = runner.run(&config("cli:claude")).unwrap();
    let RunOutcome::Succeeded { final_text, .. } = outcome else {
        panic!("expected success, got {outcome:?}");
    };
    assert!(final_text.contains("visible"), "{final_text:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_missing_workspace_is_a_failed_turn_not_the_harness_cwd() {
    let mut cfg = config("cli:claude");
    cfg.workspace = std::env::temp_dir().join("susi-adaptive-no-such-workspace-dir");
    let runner = CliSubRunner::with_command(sort_command());
    let RunOutcome::Failed { message } = runner.run(&cfg).unwrap() else {
        panic!("expected failure");
    };
    assert!(message.contains("workspace"), "{message}");
}

#[test]
fn a_program_that_does_not_exist_is_not_found_not_an_error() {
    let missing = CliCommand {
        program: PathBuf::from("susi-definitely-not-installed-cli"),
        args: Vec::new(),
        pipe_prompt_on_stdin: true,
    };
    let outcome = run_piped(
        &missing,
        &std::env::temp_dir(),
        "x",
        Duration::from_secs(5),
        DEFAULT_POLL_INTERVAL,
    );
    assert_eq!(outcome, PipedRun::NotFound);
    let runner = CliSubRunner::with_command(missing);
    assert!(matches!(
        runner.run(&config("cli:claude")).unwrap(),
        RunOutcome::Failed { .. }
    ));
}

#[test]
fn failure_tails_are_bounded_on_a_char_boundary() {
    let long = "\u{e9}".repeat(FAILURE_TAIL_CHARS + 50);
    assert_eq!(tail(&long).chars().count(), FAILURE_TAIL_CHARS);
    assert_eq!(tail("short"), "short");
}

#[test]
fn socket_endpoints_default_to_the_susi_os_ports_and_shift_by_offset() {
    let default = SusiSocketEndpoints::default();
    assert_eq!(default.gmcp_http_url, "http://127.0.0.1:9090");
    assert_eq!(
        default.gemi_openai_url,
        "http://127.0.0.1:9091/v1/chat/completions"
    );
    assert_eq!(default.gawd_a2a_url, "http://127.0.0.1:9094");
    let shifted = SusiSocketEndpoints::from_port_offset(100);
    assert_eq!(shifted.gmcp_http_url, "http://127.0.0.1:9190");
    assert_eq!(
        shifted.gemi_openai_url,
        "http://127.0.0.1:9191/v1/chat/completions"
    );
    assert_eq!(shifted.gawd_a2a_url, "http://127.0.0.1:9194");
    let saturated = SusiSocketEndpoints::from_port_offset(u16::MAX);
    assert_eq!(saturated.gmcp_http_url, "http://127.0.0.1:65535");
}

#[test]
fn execution_modes_have_stable_names_and_the_default_runner_is_auto() {
    assert_eq!(ExecutionMode::PaygByok.as_str(), "payg-byok");
    assert_eq!(ExecutionMode::MonthlySubCli.as_str(), "monthly-sub-cli");
    let runner = AdaptiveRunner::default();
    assert_eq!(runner.strategy, AdaptiveStrategy::Auto);
    assert_eq!(
        runner.plan("cli:claude").primary,
        (ExecutionMode::MonthlySubCli, "cli:claude".to_string())
    );
    assert_eq!(
        runner.plan("deepseek-flash").primary,
        (ExecutionMode::PaygByok, "deepseek-flash".to_string())
    );
}
