// Tests use assertions and unwraps to report invalid repository fixtures directly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Prevent newly malformed task records without laundering historical debt.
//! The legacy snapshot permits only known invalid content (ignoring CRLF vs LF).
//! It is not acceptance evidence: the post-merge Task parser remains strict.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use susi_gawd::admin::tasks::{accept_allowed, Task};

fn validation_error(bytes: &[u8]) -> Option<String> {
    match serde_json::from_slice::<Task>(bytes) {
        Err(error) => Some(error.to_string()),
        Ok(task) if !accept_allowed(&task.accept.cmd) => {
            Some("accept.cmd must be an allowed argv, not a list of shell commands".into())
        }
        Ok(_) => None,
    }
}

fn digest(bytes: &[u8]) -> String {
    // Git may check text out with CRLF on Windows. Ignore only that conversion;
    // every other byte change still requires repairing the invalid record.
    let mut hash = Sha256::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if let Some(content) = line.strip_suffix(b"\r\n") {
            hash.update(content);
            hash.update(b"\n");
        } else {
            hash.update(line);
        }
    }
    hex::encode(hash.finalize())
}

/// True means unchanged historical debt, false means a valid task record.
fn check_record(path: &str, bytes: &[u8], debt: &BTreeMap<String, String>) -> Result<bool, String> {
    let Some(error) = validation_error(bytes) else {
        return Ok(false);
    };
    let hash = digest(bytes);
    if debt.get(path) == Some(&hash) {
        Ok(true)
    } else {
        Err(format!("{path} [{hash}]: {error}"))
    }
}

#[test]
fn task_records_do_not_gain_new_or_modified_schema_debt() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let snapshot: serde_json::Value =
        serde_json::from_str(include_str!("task_record_schema_debt.json")).unwrap();
    assert_eq!(snapshot["schema"], "susi/task-schema-debt/v1");
    let debt: BTreeMap<String, String> =
        serde_json::from_value(snapshot["records"].clone()).unwrap();
    let mut seen_debt = BTreeSet::new();
    let mut unexpected = Vec::new();
    // These are the two directories the production task queue reads. Other
    // planning documents under .agents/tasks/ are not runtime task records.
    for relative in [".agents/tasks", ".agents/tasks/done"] {
        for entry in std::fs::read_dir(root.join(relative)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path).unwrap();
            match check_record(&relative, &bytes, &debt) {
                Ok(true) => {
                    seen_debt.insert(relative);
                }
                Ok(false) => {}
                Err(error) => unexpected.push(error),
            }
        }
    }
    unexpected.sort();
    assert!(
        unexpected.is_empty(),
        "new or modified invalid task records; repair their provenance and argv, never expand the legacy snapshot:\n{}",
        unexpected.join("\n")
    );
    let stale: Vec<_> = debt
        .keys()
        .filter(|path| !seen_debt.contains(*path))
        .collect();
    assert!(
        stale.is_empty(),
        "remove repaired or deleted task records from the legacy snapshot: {stale:?}"
    );
}

fn valid_record() -> serde_json::Value {
    serde_json::json!({
        "id": "T-TEST-1",
        "title": "test record",
        "accept": {"cmd": ["cargo", "test", "--locked"]},
        "created_by": "TEST",
        "created_unix": 1
    })
}

#[test]
fn validation_requires_creation_fields_and_real_argv() {
    assert!(validation_error(&serde_json::to_vec(&valid_record()).unwrap()).is_none());
    for field in ["created_by", "created_unix"] {
        let mut record = valid_record();
        record.as_object_mut().unwrap().remove(field);
        let error = validation_error(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(error.contains(field), "{error}");
    }
    let mut record = valid_record();
    record["accept"]["cmd"] = serde_json::json!(["cargo test --locked", "cargo clippy"]);
    assert!(validation_error(&serde_json::to_vec(&record).unwrap())
        .unwrap()
        .contains("allowed argv"));
}

#[test]
fn historical_debt_only_ignores_checkout_line_endings() {
    let mut record = valid_record();
    record.as_object_mut().unwrap().remove("created_by");
    let bytes = serde_json::to_vec_pretty(&record).unwrap();
    let debt = BTreeMap::from([("legacy.json".to_string(), digest(&bytes))]);
    assert!(check_record("legacy.json", &bytes, &debt).unwrap());
    assert!(check_record("new.json", &bytes, &debt).is_err());
    let crlf = String::from_utf8(bytes.clone())
        .unwrap()
        .replace('\n', "\r\n");
    assert!(check_record("legacy.json", crlf.as_bytes(), &debt).unwrap());
    let mut lone_cr = bytes.clone();
    lone_cr.push(b'\r');
    assert!(check_record("legacy.json", &lone_cr, &debt).is_err());
    let mut changed = bytes.clone();
    changed.push(b'\n');
    assert!(check_record("legacy.json", &changed, &debt).is_err());
    let valid = serde_json::to_vec(&valid_record()).unwrap();
    assert!(!check_record("legacy.json", &valid, &debt).unwrap());
}

#[test]
fn task_record_gate_is_unconditional_and_runs_the_tests() {
    let workflow = include_str!("../.github/workflows/test.yml").replace("\r\n", "\n");
    let (_, remaining) = workflow
        .split_once("      - name: Validate task record schemas\n")
        .expect("the branch gate must validate task-only changes");
    let step = remaining.split("\n      - name:").next().unwrap();
    assert!(
        !step
            .lines()
            .any(|line| line.trim_start().starts_with("if:")),
        "task schema validation must run even when no crate changed"
    );
    let commands: Vec<_> = step
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("run: "))
        .collect();
    assert_eq!(
        commands,
        ["cargo test --locked -p susi --test task_records"],
        "the schema guard must execute its tests without a filter or --no-run"
    );
}
