use super::*;
use crate::balance::{ProviderBudget, Source};

fn budgets(entries: &[(&str, Option<u64>)]) -> Budgets {
    Budgets {
        providers: entries
            .iter()
            .map(|(provider, micros)| ProviderBudget {
                provider: (*provider).to_string(),
                remaining: micros.map(MicroUsd::from_micros),
                source: Source::Api,
                note: "test".to_string(),
                working: false,
                absent: Vec::new(),
                expires_day: None,
                key_refused: false,
            })
            .collect(),
    }
}

/// The observed failure: `--workers gemini-flash,deepseek-flash,...` with
/// only DEEPSEEK_API_KEY set kept gemini-flash, and every task on it lost
/// two attempts to the gateway's 502 before a DeepSeek model ran.
#[test]
fn an_explicit_list_leaves_off_a_model_whose_provider_has_no_key() {
    // Only DeepSeek is keyed: Gemini has no budget entry at all.
    let gate = budgets(&[("deepseek", Some(5_000_000))]);
    let fleet = resolve_fleet(
        "gemini-flash,deepseek-flash,deepseek-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &Evidence::default(),
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["deepseek-flash", "deepseek-pro"]);
    assert_eq!(fleet.task_workers(), vec!["deepseek-flash"]);
    assert!(
        fleet.left_off[0].contains("gemini-flash left off: none of GEMINI_API_KEY"),
        "{:?}",
        fleet.left_off
    );
}

/// `susi harness gateway --upstream <proxy>` holds the keys, and `run`
/// executes where no DEEPSEEK_API_KEY is set: every listed model was left
/// off with "none of DEEPSEEK_API_KEY is set" and the run refused to
/// start, although the gateway would have served every call.
#[test]
fn a_model_without_a_key_here_stays_when_the_gateway_answers_for_it() {
    let no_keys = Budgets::default();
    let asked = Mutex::new(Vec::new());
    let routes = |model: &str| -> Result<(), String> {
        asked.lock().unwrap().push(model.to_string());
        Ok(())
    };
    let fleet = resolve_fleet_through(
        "deepseek-flash,deepseek-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &no_keys,
        &Evidence::default(),
        Some(&routes),
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["deepseek-flash", "deepseek-pro"]);
    assert!(fleet.left_off.is_empty(), "{:?}", fleet.left_off);
    assert_eq!(
        *asked.lock().unwrap(),
        vec!["deepseek-flash", "deepseek-pro"]
    );

    // A direct gateway without the key cannot route it either: the model
    // is left off, as when no gateway is asked.
    let direct = |_: &str| -> Result<(), String> {
        Err("the gateway at http://gw cannot route it either".to_string())
    };
    let error = resolve_fleet_through(
        "deepseek-flash",
        || unreachable!("an explicit list needs no catalogue"),
        &no_keys,
        &Evidence::default(),
        Some(&direct),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains(
            "deepseek-flash left off: none of DEEPSEEK_API_KEY is set here, and the gateway at http://gw cannot route it either"
        ),
        "{error}"
    );

    // A model whose key is set here is never asked about.
    let keyed = budgets(&[("deepseek", Some(5_000_000))]);
    let never = |model: &str| -> Result<(), String> {
        panic!("{model} has a key here; the gateway is not asked")
    };
    let fleet = resolve_fleet_through(
        "deepseek-flash",
        || unreachable!("an explicit list needs no catalogue"),
        &keyed,
        &Evidence::default(),
        Some(&never),
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["deepseek-flash"]);
}

/// One answer from a stub gateway, after reading the request.
fn gateway_answering(status: &'static str, body: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    format!("http://{address}/")
}

#[test]
fn the_gateway_is_asked_whether_it_can_route_a_model() {
    let ok = gateway_answering(
        "200 OK",
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#,
    );
    assert_eq!(gateway_answers(&ok, "deepseek-flash"), Ok(()));
    // What a direct gateway answers when it has no key for the model.
    let unrouted = gateway_answering(
        "502 Bad Gateway",
        r#"{"error":{"message":"resolve route: none of DEEPSEEK_API_KEY are set"}}"#,
    );
    let why = gateway_answers(&unrouted, "deepseek-flash").unwrap_err();
    assert!(why.contains("cannot route it either"), "{why}");
    let refused = gateway_answering("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#);
    let why = gateway_answers(&refused, "deepseek-flash").unwrap_err();
    assert!(why.contains("refused"), "{why}");
    // A rate limit means the gateway routed the call: the model stays.
    let busy = gateway_answering(
        "429 Too Many Requests",
        r#"{"error":{"message":"slow down"}}"#,
    );
    assert_eq!(gateway_answers(&busy, "deepseek-flash"), Ok(()));
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let why = gateway_answers(&closed, "deepseek-flash").unwrap_err();
    assert!(why.contains("did not answer"), "{why}");
}

/// A model refused before the run started was off the fleet and out of
/// every task's ladder, so the before-each-task probe never reached it:
/// topped up mid-mission, it stayed unused until the next run.
#[test]
fn a_model_left_off_for_its_refusal_alone_is_kept_in_reserve() {
    let gate = budgets(&[("deepseek", Some(50_000_000))]);
    let mut evidence = Evidence::default();
    evidence.failed.insert("deepseek-flash".to_string(), 1);
    // Gemini has no key here: refused or not, no probe can reach it.
    evidence.failed.insert("gemini-flash".to_string(), 1);
    let fleet = resolve_fleet(
        "deepseek-flash,gemini-flash,deepseek-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &evidence,
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["deepseek-pro"]);
    assert_eq!(fleet.standby, vec!["deepseek-flash"]);

    let auto = Registry::builtin().auto_config_ranked(
        &|name| name == "DEEPSEEK_API_KEY",
        &PriceCatalogue::builtin(),
        UsageProfile::agent(),
        &ProviderGate {
            active: &|_| true,
            use_first: &|_| None,
            prefer: &|_| None,
            latency: &|_| None,
            live: &|model| !evidence.failed.contains_key(model),
        },
    );
    assert_eq!(auto.ladder(), vec!["deepseek-pro"]);
    assert_eq!(
        auto_standby(&auto, &gate, &evidence),
        vec!["deepseek-flash"]
    );
    // A model its provider no longer lists is not waiting for a probe.
    let mut gone = gate.clone();
    gone.providers[0].absent = vec!["deepseek-flash".to_string()];
    assert!(auto_standby(&auto, &gone, &evidence).is_empty());
}

/// `run` drives a gateway that holds the DeepSeek key (an explicit
/// `--upstream`, or `--api-key-env`) from a shell without it. One refused
/// call left deepseek-flash off "until a probe re-admits it", but a probe
/// from here needs the key here, so none was ever sent, at run start or
/// mid-mission, and the model was off for every later run.
#[test]
fn a_refused_model_without_a_key_here_comes_back_when_the_gateway_answers() {
    let no_keys = Budgets::default();
    let no_key_here = |_: &str| false;
    let now = 10 * PROBE_AFTER_MS;
    let mut evidence = Evidence::default();
    // Refused long enough ago to be probed, and refused just now.
    evidence
        .failed
        .insert("deepseek-flash".to_string(), now - PROBE_AFTER_MS);
    evidence.failed.insert("deepseek-pro".to_string(), now - 1);
    let asked = Mutex::new(Vec::new());
    let answers = |model: &str| -> Result<(), String> {
        asked.lock().unwrap().push(model.to_string());
        Ok(())
    };
    let fleet = explicit_fleet(
        "deepseek-flash,deepseek-pro",
        &no_keys,
        &evidence,
        &Here {
            gateway: Some(&answers),
            key_set: &no_key_here,
            now_unix_ms: now,
        },
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["deepseek-flash"]);
    assert_eq!(
        *asked.lock().unwrap(),
        vec!["deepseek-flash"],
        "a refusal under five minutes old is not asked about yet"
    );
    assert!(
        fleet.standby.is_empty(),
        "no probe from here reaches either"
    );
    assert_eq!(fleet.left_off.len(), 1, "{:?}", fleet.left_off);
    let young = &fleet.left_off[0];
    assert!(
        young.starts_with("deepseek-pro left off")
            && young.contains("none of DEEPSEEK_API_KEY is set here")
            && young.contains(
                "a command that drives the gateway asks it once the refusal is 5 minutes old"
            ),
        "{young}"
    );
    assert!(!young.contains("a probe re-admits it"), "{young}");

    // Refused again through the gateway: off, with the gateway's reason,
    // and no promise of a probe that cannot be sent.
    let refuses = |_: &str| -> Result<(), String> {
        Err("the gateway's call to it was refused (HTTP 402: key refused or no credit)".into())
    };
    let error = explicit_fleet(
        "deepseek-flash",
        &no_keys,
        &evidence,
        &Here {
            gateway: Some(&refuses),
            key_set: &no_key_here,
            now_unix_ms: now,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains(
            "deepseek-flash left off: its last real call was refused (key refused or no credit), none of DEEPSEEK_API_KEY is set here, and the gateway's call to it was refused"
        ),
        "{error}"
    );
    assert!(!error.contains("a probe re-admits it"), "{error}");

    // With no gateway to ask, nothing this command sends can re-admit it.
    let error = explicit_fleet(
        "deepseek-flash",
        &no_keys,
        &evidence,
        &Here {
            gateway: None,
            key_set: &no_key_here,
            now_unix_ms: now,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("no probe from here can re-admit it until the key is set here"),
        "{error}"
    );
    assert!(!error.contains("a probe re-admits it"), "{error}");
}

/// A name the registry cannot resolve goes to OpenAI, so a probe from
/// here needs OPENAI_API_KEY here: a proxy's own model name, refused once
/// through a gateway on an explicit upstream, waited in reserve for a
/// probe that was never sent.
#[test]
fn a_refused_unregistered_model_is_probed_from_here_only_with_the_openai_key_here() {
    let no_keys = Budgets::default();
    let now = 10 * PROBE_AFTER_MS;
    let mut evidence = Evidence::default();
    evidence
        .failed
        .insert("proxy-model".to_string(), now - PROBE_AFTER_MS);
    let asked = Mutex::new(Vec::new());
    let answers = |model: &str| -> Result<(), String> {
        asked.lock().unwrap().push(model.to_string());
        Ok(())
    };
    let fleet = explicit_fleet(
        "proxy-model",
        &no_keys,
        &evidence,
        &Here {
            gateway: Some(&answers),
            key_set: &|_| false,
            now_unix_ms: now,
        },
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["proxy-model"]);
    assert_eq!(*asked.lock().unwrap(), vec!["proxy-model"]);

    asked.lock().unwrap().clear();
    let fleet = explicit_fleet(
        "proxy-model,proxy-other",
        &no_keys,
        &evidence,
        &Here {
            gateway: Some(&answers),
            key_set: &|name| name == "OPENAI_API_KEY",
            now_unix_ms: now,
        },
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["proxy-other"]);
    assert_eq!(fleet.standby, vec!["proxy-model"]);
    assert!(fleet.left_off[0].contains("a probe re-admits it"));
    assert!(
        asked.lock().unwrap().is_empty(),
        "a probe from here reaches it"
    );
}

/// Two tasks of one batch re-admit the same refused model at once. The
/// one that lost the race to probe it got no answer, ordered its ladder
/// from the stale ledger, found every model refused and stopped the
/// mission while the other task ran on the model the probe re-admitted.
#[test]
fn a_task_that_loses_the_probe_race_waits_for_the_answer() {
    let model = "probe-race-model".to_string();
    let calls = AtomicUsize::new(0);
    let (started, probing) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let started = Mutex::new(started);
    let released = Mutex::new(released);
    let slow = |_: &str| -> ProbeAnswer {
        calls.fetch_add(1, Ordering::SeqCst);
        started.lock().unwrap().send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
        Err(crate::ledger::FailureClass::NoCredit)
    };
    let fast = |_: &str| -> ProbeAnswer {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    std::thread::scope(|scope| {
        let winner = scope.spawn(|| probe_all_with(std::slice::from_ref(&model), &slow));
        probing.recv().unwrap();
        let loser = scope.spawn(|| probe_all_with(std::slice::from_ref(&model), &fast));
        // Hold the winner's probe until the loser waits on it, or has
        // returned without it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let waiting = PROBING
                .lock()
                .unwrap()
                .get(&model)
                .is_some_and(|slot| slot.waiters.load(Ordering::SeqCst) > 0);
            if waiting || loser.is_finished() || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        release.send(()).unwrap();
        let answer = vec![(model.clone(), Err(crate::ledger::FailureClass::NoCredit))];
        assert_eq!(winner.join().unwrap(), answer);
        assert_eq!(
            loser.join().unwrap(),
            answer,
            "the loser takes the in-flight probe's answer"
        );
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one model, one probe");
    assert!(PROBING.lock().unwrap().get(&model).is_none());
}

/// One answer per model even when the probing thread gives none: a
/// waiter is released, and nothing is reported for the model.
#[test]
fn a_probe_that_panics_releases_its_waiters() {
    let model = "probe-panic-model".to_string();
    let answers = probe_all_with(std::slice::from_ref(&model), &|_| -> ProbeAnswer {
        panic!("the probe call panicked")
    });
    assert!(answers.is_empty());
    assert!(PROBING.lock().unwrap().get(&model).is_none());
    // The next probe of the model is not stuck behind the lost one.
    assert_eq!(
        probe_all_with(std::slice::from_ref(&model), &|_| Ok(())),
        vec![(model.clone(), Ok(()))]
    );
}

/// One 429 used to drop the model from an explicit list until a probe
/// five minutes later; with one model, `up` and `run` refused to start.
#[test]
fn a_transient_failure_keeps_the_model_and_a_refusal_through_an_alias_drops_it() {
    let gate = budgets(&[("deepseek", Some(50_000_000))]);
    let mut evidence = Evidence::default();
    evidence.cooling.insert("deepseek-flash".to_string(), 1);
    let kept = resolve_fleet(
        "deepseek-flash",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &evidence,
    )
    .unwrap();
    assert_eq!(kept.ladder, vec!["deepseek-flash"]);
    let mut refused = Evidence::default();
    refused.failed.insert("deepseek-flash".to_string(), 1);
    let error = resolve_fleet(
        "coder",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &refused,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("coder left off: its last real call was refused"),
        "{error}"
    );
}

#[test]
fn an_explicit_list_leaves_off_a_model_its_provider_no_longer_lists() {
    let mut gate = budgets(&[("gemini", None)]);
    gate.providers[0].working = true;
    gate.providers[0].absent = vec!["gemini-pro".to_string()];
    let fleet = resolve_fleet(
        "gemini-flash,gemini-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &Evidence::default(),
    )
    .unwrap();
    assert_eq!(fleet.ladder, vec!["gemini-flash"]);
    assert!(
        fleet.left_off[0].contains("no longer lists"),
        "{:?}",
        fleet.left_off
    );
}

/// A model the registry cannot resolve goes to OpenAI; once refused, it
/// was never probed again, so it stayed off every explicit list.
#[test]
fn a_refused_unregistered_model_is_probed_once_the_refusal_is_old() {
    let registry = Registry::builtin();
    let openai_only = |name: &str| name == "OPENAI_API_KEY";
    assert!(routable("gpt-4o", registry, &openai_only));
    assert!(!routable("gpt-4o", registry, &|_| false));
    assert!(routable("gpt-luna", registry, &openai_only));
    assert!(!routable("deepseek-flash", registry, &openai_only));
    let failures = BTreeMap::from([
        (
            "gpt-4o".to_string(),
            crate::policy::ModelFailure {
                at_unix_ms: 1_000,
                class: crate::ledger::FailureClass::NoCredit,
            },
        ),
        (
            "busy".to_string(),
            crate::policy::ModelFailure {
                at_unix_ms: 1_000,
                class: crate::ledger::FailureClass::RateLimited,
            },
        ),
    ]);
    let reach = |model: &str| routable(model, registry, &openai_only);
    assert!(due_for_probe(&failures, 1_000 + PROBE_AFTER_MS - 1, &reach).is_empty());
    assert_eq!(
        due_for_probe(&failures, 1_000 + PROBE_AFTER_MS, &reach),
        vec!["gpt-4o".to_string()],
        "a transient failure cools down on its own and is not probed"
    );
}

/// A probe that hit a rate limit leaves the model eligible, so the report
/// must not call it inactive; only a refused probe keeps a model off.
#[test]
fn a_probe_that_failed_transiently_is_not_reported_as_inactive() {
    let evidence = Evidence {
        failed: BTreeMap::from([("refused".to_string(), 1)]),
        cooling: BTreeMap::from([("busy".to_string(), 1)]),
        probed: vec![
            ("healed".to_string(), true),
            ("refused".to_string(), false),
            ("busy".to_string(), false),
        ],
        ..Evidence::default()
    };
    assert_eq!(
        probe_report(&evidence),
        vec![
            "probe: healed answered; active again",
            "probe: refused failed again; it stays inactive",
            "probe: busy failed transiently; it stays on the ladder, tried after the others",
        ]
    );
}

#[test]
fn an_unpriced_model_runs_last_in_its_tier_even_when_it_caches() {
    // deepseek-flash caches; kimi-flash does not. With deepseek-flash
    // unpriced, the cache key sorted it first.
    let mut catalogue = PriceCatalogue::builtin();
    catalogue.models.remove("deepseek-flash");
    let auto = Registry::builtin().auto_config_ranked(
        &|name| name == "DEEPSEEK_API_KEY" || name == "MOONSHOT_API_KEY",
        &catalogue,
        UsageProfile::agent(),
        &ProviderGate {
            active: &|_| true,
            use_first: &|_| None,
            prefer: &|_| None,
            latency: &|_| None,
            live: &|_| true,
        },
    );
    assert_eq!(auto.unpriced, vec!["deepseek-flash"]);
    let ranked = unpriced_last(auto);
    assert_eq!(
        ranked.workers.last().map(String::as_str),
        Some("deepseek-flash")
    );
    assert_eq!(
        ranked.workers.first().map(String::as_str),
        Some("kimi-flash")
    );
    assert_eq!(ranked.orchestrator.as_deref(), Some("deepseek-pro"));
}

#[test]
fn a_fresh_susi_checkout_keeps_its_own_ledger_before_the_first_write() {
    let home = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(checkout.path().join("deploy/harness")).unwrap();
    std::fs::write(checkout.path().join("deploy/harness/prices.json"), "{}").unwrap();
    assert_eq!(
        ledger_for(checkout.path(), None, home.path()),
        checkout.path().join("out").join("ledger.jsonl")
    );
    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(
        ledger_for(elsewhere.path(), None, home.path()),
        home.path().join("harness").join("ledger.jsonl")
    );
    assert_eq!(
        ledger_for(checkout.path(), Some("x.jsonl".to_string()), home.path()),
        std::path::PathBuf::from("x.jsonl")
    );
}

#[test]
fn an_explicit_list_drives_with_its_strong_model_and_drops_inactive_ones() {
    let gate = budgets(&[("deepseek", Some(5_000_000)), ("gemini", Some(10))]);
    let fleet = resolve_fleet(
        "gemini-flash, deepseek-flash,deepseek-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &Evidence::default(),
    )
    .unwrap();
    assert_eq!(fleet.orchestrator.as_deref(), Some("deepseek-pro"));
    assert_eq!(fleet.ladder, vec!["deepseek-flash", "deepseek-pro"]);
    assert_eq!(fleet.left_off.len(), 1);
    assert!(fleet.left_off[0].contains("gemini-flash"));
}

#[test]
fn an_explicit_list_with_nothing_active_is_an_error() {
    let gate = budgets(&[("gemini", Some(10))]);
    let error = resolve_fleet(
        "gemini-flash",
        || unreachable!(),
        &gate,
        &Evidence::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("inactive"));
}

#[test]
fn a_model_whose_last_call_failed_is_inactive_whatever_its_budget() {
    // DeepSeek has plenty of budget, but deepseek-flash's last real call
    // failed: the call is the truth.
    let gate = budgets(&[("deepseek", Some(50_000_000))]);
    let mut evidence = Evidence::default();
    evidence.failed.insert("deepseek-flash".to_string(), 1);
    let listed = resolve_fleet(
        "deepseek-flash,deepseek-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &evidence,
    )
    .unwrap();
    assert_eq!(listed.ladder, vec!["deepseek-pro"]);
    assert!(listed.left_off[0].contains("last real call was refused"));
    let auto = Registry::builtin().auto_config_ranked(
        &|name| name == "DEEPSEEK_API_KEY",
        &PriceCatalogue::builtin(),
        UsageProfile::agent(),
        &ProviderGate {
            active: &|_| true,
            use_first: &|_| None,
            prefer: &|_| None,
            latency: &|_| None,
            live: &|model| !evidence.failed.contains_key(model),
        },
    );
    assert_eq!(auto.ladder(), vec!["deepseek-pro"]);
    assert_eq!(auto.failed, vec!["deepseek-flash"]);
}

#[test]
fn an_explicit_list_takes_its_roles_from_the_registry() {
    let gate = budgets(&[("deepseek", Some(50_000_000)), ("gemini", Some(50_000_000))]);
    let fleet = resolve_fleet(
        "gemini-flash,deepseek-flash,deepseek-pro",
        || unreachable!("an explicit list needs no catalogue"),
        &gate,
        &Evidence::default(),
    )
    .unwrap();
    assert_eq!(fleet.orchestrator.as_deref(), Some("deepseek-pro"));
    assert_eq!(fleet.workers, vec!["gemini-flash", "deepseek-flash"]);
    assert_eq!(fleet.fixers, vec!["deepseek-pro"]);
    assert_eq!(fleet.task_workers(), vec!["gemini-flash", "deepseek-flash"]);
}

#[test]
fn auto_model_takes_the_fleets_orchestrator() {
    let fleet = Fleet {
        orchestrator: Some("deepseek-pro".to_string()),
        ladder: vec!["deepseek-pro".to_string()],
        ..Fleet::default()
    };
    assert_eq!(resolve_model("auto", &fleet).unwrap(), "deepseek-pro");
    assert_eq!(resolve_model("claude-opus", &fleet).unwrap(), "claude-opus");
    assert!(resolve_model("auto", &Fleet::default()).is_err());
}

#[test]
fn key_names_come_from_the_registry() {
    let names = key_names(Registry::builtin());
    for name in [
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "MOONSHOT_API_KEY",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(names.contains(&name), "{name} missing from {names:?}");
    }
}

#[test]
fn the_environment_report_never_shows_a_key() {
    let variables = environment();
    let keys = key_names(Registry::builtin());
    for variable in &variables {
        if keys.contains(&variable.name.as_str()) {
            assert_eq!(variable.value, None, "{} must not be shown", variable.name);
        }
    }
    for name in [
        "SUSI_HARNESS_BUDGET_ANTHROPIC_USD",
        "SUSI_HARNESS_CREDIT_EXPIRES_ANTHROPIC",
        "SUSI_HARNESS_RPM_DEEPSEEK",
        "SUSI_HARNESS_TPM_GEMINI",
        crate::providers::OVERLAY_ENV,
        crate::providers::PREFER_ENV,
    ] {
        assert!(variables.iter().any(|v| v.name == name), "{name} missing");
    }
}

#[test]
fn the_builtin_catalogue_prices_the_ladder() {
    let catalogue = PriceCatalogue::builtin();
    assert!(!catalogue.is_empty());
    for model in Registry::default_worker_ladder() {
        assert!(catalogue.price(model).is_some(), "{model} unpriced");
    }
    assert!(
        PriceCatalogue::load_or_builtin(Path::new("no/such/prices.json"))
            .unwrap()
            .len()
            == catalogue.len()
    );
}
