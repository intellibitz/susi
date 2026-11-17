use super::*;
use crate::plane_bus::{PlaneBus, PlaneHandler};
use std::sync::{Arc, Mutex};

fn isolated(name: &str) -> bool {
    const CHILD: &str = "SUSI_LEDGER_SOCKET_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        crate::susi_error::enter_service_mode();
        return false;
    }
    let home = susi_paths::tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            &format!("commit_log::socket::tests::{name}"),
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, name)
        .current_dir(home.path());
    susi_paths::test_env::isolate_home(&mut child, home.path());
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        child.creation_flags(0x08000000);
    }
    let result = child.output().unwrap();
    assert!(
        result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    true
}

struct Host {
    reply: Mutex<Value>,
    requests: Mutex<Vec<Value>>,
}

impl PlaneHandler for Host {
    fn handle(&self, topic: &str, request: Value) -> Result<Value, String> {
        assert_eq!(topic, "fleet.cluster.v1");
        self.requests.lock().unwrap().push(request);
        Ok(self.reply.lock().unwrap().clone())
    }
}

fn host(reply: Value) -> Arc<Host> {
    let host = Arc::new(Host {
        reply: Mutex::new(reply),
        requests: Mutex::new(Vec::new()),
    });
    PlaneBus::global().register("fleet.cluster.v1", host.clone());
    host
}

fn record() -> CommitRecord {
    serde_json::from_value(
        json!({"epoch":"fixture", "coordinator":"fixture", "electorate":[],
        "tally":0, "quorum_threshold":0, "value_hash":"fixture", "value":"fixture",
        "committed_at":1, "signature":"fixture"}),
    )
    .unwrap()
}

#[test]
fn ledger_socket_preserves_paths_records_outcomes_and_one_request() {
    if isolated("ledger_socket_preserves_paths_records_outcomes_and_one_request") {
        return;
    }
    let host = host(json!({"result":"appended"}));
    let target = Path::new("nested/ledger.jsonl");
    let record = record();
    append(target, &record).unwrap();
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["action"], "ledger_v1");
    assert_eq!(
        requests[0]["args"]["home"],
        serde_json::to_value(susi_paths::SusiDirs::config_dir()).unwrap()
    );
    assert_eq!(
        requests[0]["args"]["request"]["record"],
        serde_json::to_value(&record).unwrap()
    );
    assert_eq!(
        requests[0]["args"]["request"]["path"],
        json!({"encoding":"utf8", "value":std::path::absolute(target).unwrap()})
    );
    drop(requests);
    *host.reply.lock().unwrap() =
        json!({"result":"batch", "outcomes":["applied","skipped","refused"]});
    assert_eq!(
        append_many(target, &[record.clone(), record.clone(), record.clone()]),
        [
            AppendOutcome::Applied,
            AppendOutcome::Skipped,
            AppendOutcome::Refused
        ]
    );
    *host.reply.lock().unwrap() = json!({"result":"compacted", "archived":2});
    assert_eq!(
        compact(
            target,
            Path::new("other/snapshot"),
            Path::new("archive/history")
        )
        .unwrap(),
        2
    );
    assert_eq!(host.requests.lock().unwrap().len(), 3);
}

#[test]
fn ledger_socket_preserves_typed_refusals_and_rejects_malformed_replies() {
    if isolated("ledger_socket_preserves_typed_refusals_and_rejects_malformed_replies") {
        return;
    }
    let expected = EaiError::authorization("fixture membership refusal");
    let host = host(
        json!({"result":"refused", "kind":expected.kind_name(), "code":expected.code(),
        "retryable":expected.retryable(), "message":expected.to_string()}),
    );
    let error = append(Path::new("ledger"), &record()).unwrap_err();
    assert_eq!(error.kind_name(), expected.kind_name());
    assert_eq!(error.code(), expected.code());
    assert_eq!(error.retryable(), expected.retryable());
    assert_eq!(error.to_string(), expected.to_string());
    let fallback = crate::susi_error::rewrap("unknown-error-kind", "unknown".into());
    *host.reply.lock().unwrap() = json!({"result":"refused", "kind":"unknown-error-kind", "code":fallback.code(), "retryable":fallback.retryable(), "message":"unknown"});
    assert_eq!(
        append(Path::new("ledger"), &record())
            .unwrap_err()
            .kind_name(),
        "Protocol"
    );
    *host.reply.lock().unwrap() = json!({"result":"batch", "outcomes":[]});
    assert_eq!(
        append_many(Path::new("ledger"), &[record()]),
        [AppendOutcome::Refused]
    );
    *host.reply.lock().unwrap() = json!({"result":"compacted", "archived":-1});
    assert_eq!(
        compact(
            Path::new("ledger"),
            Path::new("snapshot"),
            Path::new("archive")
        )
        .unwrap_err()
        .kind_name(),
        "Protocol"
    );
    assert_eq!(host.requests.lock().unwrap().len(), 4);
}

#[test]
fn ledger_socket_absent_provider_names_the_serving_command() {
    if isolated("ledger_socket_absent_provider_names_the_serving_command") {
        return;
    }
    let error = append(Path::new("ledger"), &record()).unwrap_err();
    assert_eq!(error.kind_name(), "Config");
    assert!(error.to_string().contains("susi-plug-host"));
    assert!(!Path::new("ledger").exists());
}

/// Executed by commercial's real-socket paired test against this exact test
/// binary. It is explicitly ignored in standalone runs because it requires the
/// separately hosted implementation; ordinary contract tests above need none.
#[test]
#[ignore = "run with the Fleet real-socket parity fixture"]
fn ledger_socket_external_parity_probe() {
    crate::susi_error::enter_service_mode();
    let request: Value =
        serde_json::from_str(&std::env::var("SUSI_LEDGER_PROBE_REQUEST").unwrap()).unwrap();
    let target = std::path::PathBuf::from(request["path"].as_str().unwrap());
    let result = match request["op"].as_str().unwrap() {
        "append" => super::super::append_to(&target, &serde_json::from_value(request["record"].clone()).unwrap()).map(|()| json!({"result":"appended"})),
        "batch" => Ok(json!({"outcomes":super::super::append_many_to(&target, &serde_json::from_value::<Vec<CommitRecord>>(request["records"].clone()).unwrap()).iter().map(|outcome| match outcome {
            AppendOutcome::Applied => "applied", AppendOutcome::Skipped => "skipped", AppendOutcome::Refused => "refused",
        }).collect::<Vec<_>>()})),
        "compact" => super::super::compact_at(&target, Path::new(request["snapshot"].as_str().unwrap()), Path::new(request["archive"].as_str().unwrap())).map(|archived| json!({"archived":archived})),
        other => panic!("unknown probe operation {other}"),
    }.unwrap_or_else(|error| json!({"error":error.to_string(), "kind":error.kind_name(), "code":error.code(), "retryable":error.retryable()}));
    println!("LEDGER_PROBE_RESULT={result}");
}
