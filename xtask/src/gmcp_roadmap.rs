//! Native port of `scripts/check-gmcp-roadmap.sh`, not a stronger roadmap schema.
//!
//! Keep the shell source as a parity reference. In particular it counts matching
//! task records, not unique requirements, and does not detect indirect cycles.
//!
//! Native usage (the optional final argument selects another repository root):
//! `cargo run --offline --locked -p xtask --bin check-gmcp-roadmap -- [root]`.
//! The `gmcp_roadmap` integration test calls this same validator directly.
//! It launches no shell, jq, Python, or other subprocess. Validation, JSON, and
//! I/O failures produce a nonzero CLI exit status.
//!
//! Source-backed parity coverage:
//! - `vectors`, `from_entries`, `range(1; 49)`: an array containing all 48 IDs;
//!   the last duplicate object wins; other vectors may satisfy dependencies.
//! - `depends_on`, `index($vid)`, `$by_id[.]`: arrays, no direct self-reference,
//!   and no unknown dependency. Indirect cycles remain outside these checks.
//! - Priority comparisons, `length`, `startswith`: P0/P1/P2, nonempty type/name,
//!   mastery length at least 160, and explicit progress prefixes. Unicode scalar
//!   lengths and jq's accepted numeric/collection lengths are preserved.
//! - `unique | sort` on VC-203-048: all preceding 47 vectors directly, ignoring
//!   dependency order and duplicates.
//! - Immediate open/done task JSON streams, `scan`, count: exactly one matching
//!   Requirement token per counted record, total 336. Preserve case, ASCII
//!   digits, suffixes, prefix matching, and the legacy glob/find dotfile rules.
//! - Document substring loops and `grep -Fq`: every vector, all six completion
//!   tokens, and the README link are required.
//!
//! The legacy checker does not enforce unique requirement IDs, a 001-048 range
//! on task tokens, indirect acyclicity, task dependency ordering, acceptance
//! command content, or verdict contents. Its success text claimed acyclicity
//! without checking it; the native success text omits that unsupported claim.
//! Inputs are UTF-8 JSON and documentation. The existing serde_json parser is
//! reused; malformed encodings, unsupported JSON numbers, and extra top-level
//! roadmap values fail closed. Diagnostics are not byte-for-byte jq output.
//!
//! `docs/gmcp-roadmap.md` is a pinned claim source in the requirements ledger
//! and every GMCP evidence digest. Preserve its bytes during this tooling-only
//! migration. Updating that document requires repinning its source references
//! and genuine requirement/mastery evidence, separately from structural parity.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) const SUCCESS: &str =
    "GMCP roadmap OK: 48 vectors, 336 requirement tasks, complete mastery fan-in";
const DOC_TOKENS: [&str; 6] = [
    "MCP 2026-07-28",
    "336 open implementation",
    "gmcp_complete_mastery",
    "Official conformance",
    "Standards and metric drift loop",
    "zero unresolved critical/high",
];

fn vector_id(number: usize) -> String {
    format!("VC-203-{number:03}")
}

/// Read only the two immediate task directories, as the old `find -maxdepth 1`
/// did. Slurping a JSON stream preserves multiple records per file. A null task
/// becomes an object after jq's `. + {_done: ...}`; other nonobjects fail.
fn read_tasks(root: &Path) -> Result<Vec<Value>> {
    let mut tasks = Vec::new();
    for relative in [".agents/tasks", ".agents/tasks/done"] {
        let directory = root.join(relative);
        if !directory.is_dir() {
            continue;
        }
        let mut paths = fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.retain(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".json"))
        });
        // Bash's initial compgen glob excludes dotfiles, but find includes them
        // once at least one visible *.json exists.
        if !paths.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
        }) {
            continue;
        }
        paths.sort();
        let mut bytes = Vec::new();
        for path in paths {
            bytes.extend(fs::read(path)?);
        }
        for value in serde_json::Deserializer::from_slice(&bytes).into_iter::<Value>() {
            let value = value?;
            if !value.is_object() && !value.is_null() {
                return Err(format!("{relative}: task must be an object or null").into());
            }
            tasks.push(value);
        }
    }
    Ok(tasks)
}

/// jq's `//` treats false and null as absent. Its length counts Unicode scalar
/// values, collection entries, or a number's absolute value, not UTF-8 bytes.
fn jq_length(value: &Value) -> Result<f64> {
    match value {
        Value::Null | Value::Bool(false) => Ok(0.0),
        Value::Bool(true) => Err("jq length is undefined for true".into()),
        Value::Number(number) => number
            .as_f64()
            .map(f64::abs)
            .ok_or_else(|| "number cannot be represented for jq length".into()),
        Value::String(text) => Ok(text.chars().count() as f64),
        Value::Array(values) => Ok(values.len() as f64),
        Value::Object(values) => Ok(values.len() as f64),
    }
}

fn jq_string(value: &Value) -> Result<&str> {
    match value {
        Value::Null | Value::Bool(false) => Ok(""),
        Value::String(text) => Ok(text),
        Value::Bool(true) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err("jq string operation requires a string".into())
        }
    }
}

/// Exact nonoverlapping ASCII pattern from jq scan:
/// `Requirement (GMCP-203-([0-9]{3})-(01|02|03|04|05|06|M))`.
/// There is deliberately no range check or trailing word boundary.
fn requirement_matches(goal: &str) -> usize {
    const PREFIX: &str = "Requirement GMCP-203-";
    goal.match_indices(PREFIX)
        .filter(|(at, _)| {
            let suffix = &goal.as_bytes()[at + PREFIX.len()..];
            suffix.len() >= 5
                && suffix[..3].iter().all(u8::is_ascii_digit)
                && suffix[3] == b'-'
                && (suffix[4] == b'M'
                    || (suffix.len() >= 6
                        && suffix[4] == b'0'
                        && (b'1'..=b'6').contains(&suffix[5])))
        })
        .count()
}

fn validate_values(roadmap: &Value, tasks: &[Value]) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    let Some(vectors) = roadmap["vectors"].as_array() else {
        return Ok(vec![".agents/roadmap.json vectors must be a list".into()]);
    };
    // jq's from_entries keeps the last object with each string ID, including
    // non-VC-203 vectors that may be referenced by a VC-203 dependency.
    let by_id: BTreeMap<_, _> = vectors
        .iter()
        .filter_map(|vector| vector.get("id")?.as_str().map(|id| (id, vector)))
        .collect();
    for number in 1..=48 {
        let id = vector_id(number);
        if !by_id.contains_key(id.as_str()) {
            errors.push(format!("missing VC-203 vector: {id}"));
        }
    }
    for number in 1..=48 {
        let id = vector_id(number);
        let Some(vector) = by_id.get(id.as_str()) else {
            continue;
        };
        match &vector["depends_on"] {
            Value::Array(deps) => {
                if deps.iter().any(|dep| dep.as_str() == Some(&id)) {
                    errors.push(format!("{id} depends on itself"));
                }
                for dependency in deps {
                    let dependency = dependency
                        .as_str()
                        .ok_or("jq object dependency index must be a string")?;
                    if !by_id.contains_key(dependency) {
                        errors.push(format!("{id} has unknown dependency {dependency}"));
                    }
                }
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Object(_) => errors.push(format!("{id} depends_on must be a list")),
        }
        if !matches!(vector["priority"].as_str(), Some("P0" | "P1" | "P2")) {
            errors.push(format!("{id} has invalid priority"));
        }
        for (field, message) in [
            ("type", "has no type"),
            ("vector", "has no capability name"),
        ] {
            if jq_length(&vector[field])? == 0.0 {
                errors.push(format!("{id} {message}"));
            }
        }
        if jq_length(&vector["mastery_target"])? < 160.0 {
            errors.push(format!("{id} mastery target is not sufficiently explicit"));
        }
        let progress = jq_string(&vector["progress"])?;
        if !progress.starts_with("NOT DELIVERED") && !progress.starts_with("DELIVERED") {
            errors.push(format!(
                "{id} progress must explicitly say NOT DELIVERED or DELIVERED"
            ));
        }
    }
    let fan_in: BTreeSet<_> = by_id
        .get("VC-203-048")
        .and_then(|vector| vector["depends_on"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    if fan_in != (1..48).map(vector_id).collect() {
        errors.push("VC-203-048 must depend directly on every preceding VC-203 vector".into());
    }
    let mut count = 0;
    for task in tasks {
        if !task.is_object() && !task.is_null() {
            return Err("task must be an object or null".into());
        }
        if requirement_matches(jq_string(&task["goal"])?) == 1 {
            // The legacy projection reads .accept.cmd even though no rule uses
            // it. Preserve jq's type error, but do not invent argv validation.
            if !task["accept"].is_object() && !task["accept"].is_null() {
                return Err("jq cannot read .accept.cmd from a nonobject".into());
            }
            count += 1;
        }
    }
    if count != 336 {
        errors.push(format!(
            "expected 336 unique requirement tasks, found {count}"
        ));
    }
    Ok(errors)
}

/// Run all checks without launching a shell, jq, or any other process.
pub(crate) fn validate(root: &Path) -> Result<Vec<String>> {
    let roadmap = root.join(".agents/roadmap.json");
    if !roadmap.is_file() {
        return Ok(vec![".agents/roadmap.json missing".into()]);
    }
    let roadmap: Value = serde_json::from_slice(&fs::read(roadmap)?)?;
    let tasks = read_tasks(root)?;
    let mut errors = validate_values(&roadmap, &tasks)?;
    let doc = root.join("docs/gmcp-roadmap.md");
    if !doc.is_file() {
        errors.push("docs/gmcp-roadmap.md is missing".into());
    } else {
        let text = fs::read_to_string(doc)?;
        for number in 1..=48 {
            let id = vector_id(number);
            if !text.contains(&id) {
                errors.push(format!("docs/gmcp-roadmap.md does not mention {id}"));
            }
        }
        for token in DOC_TOKENS {
            if !text.contains(token) {
                errors.push(format!(
                    "docs/gmcp-roadmap.md lacks required completion token '{token}'"
                ));
            }
        }
    }
    let readme = root.join("README.md");
    if !readme.is_file()
        || !fs::read(readme)?
            .windows(b"docs/gmcp-roadmap.md".len())
            .any(|window| window == b"docs/gmcp-roadmap.md")
    {
        errors.push("README.md does not link the GMCP completion roadmap".into());
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    // Fixture construction and assertions may panic to report a failed test.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture() -> (Value, Vec<Value>) {
        let vectors: Vec<_> = (1..=48)
            .map(|number| {
                json!({
                    "id": vector_id(number), "depends_on": if number == 48 {
                        (1..48).map(vector_id).collect::<Vec<_>>()
                    } else { vec![] },
                    "priority": "P0", "type": "PROTOCOL", "vector": "capability",
                    "mastery_target": "x".repeat(160), "progress": "NOT DELIVERED"
                })
            })
            .collect();
        let tasks = (1..=48)
            .flat_map(|number| {
                ["01", "02", "03", "04", "05", "06", "M"].map(
                    |suffix| json!({"goal": format!("Requirement GMCP-203-{number:03}-{suffix}")}),
                )
            })
            .collect();
        (json!({"vectors": vectors}), tasks)
    }

    fn errors(roadmap: &Value, tasks: &[Value]) -> Vec<String> {
        validate_values(roadmap, tasks).unwrap()
    }

    #[test]
    fn complete_fixture_and_unicode_length_boundary_pass() {
        let (mut roadmap, tasks) = fixture();
        roadmap["vectors"][0]["mastery_target"] = json!("\u{1f600}".repeat(160));
        assert!(errors(&roadmap, &tasks).is_empty());
        roadmap["vectors"][0]["mastery_target"] = json!("\u{1f600}".repeat(159));
        assert_eq!(
            errors(&roadmap, &tasks),
            ["VC-203-001 mastery target is not sufficiently explicit"]
        );
        assert!(SUCCESS.contains("336 requirement tasks"));
    }

    #[test]
    fn every_vector_is_required_and_vectors_must_be_an_array() {
        for number in 0..48 {
            let (mut roadmap, tasks) = fixture();
            roadmap["vectors"].as_array_mut().unwrap().remove(number);
            assert!(errors(&roadmap, &tasks)
                .contains(&format!("missing VC-203 vector: {}", vector_id(number + 1))));
        }
        for value in [Value::Null, json!({}), json!(true), json!("vectors")] {
            assert!(!errors(&json!({"vectors": value}), &[]).is_empty());
        }
    }

    #[test]
    fn each_vector_field_rule_rejects_its_negative_fixture() {
        let cases = [
            ("depends_on", Value::Null, "depends_on must be a list"),
            ("depends_on", json!(["VC-203-001"]), "depends on itself"),
            (
                "depends_on",
                json!(["UNKNOWN"]),
                "has unknown dependency UNKNOWN",
            ),
            ("priority", json!("P3"), "has invalid priority"),
            ("type", json!(""), "has no type"),
            ("vector", json!(""), "has no capability name"),
            (
                "mastery_target",
                json!("x".repeat(159)),
                "mastery target is not sufficiently explicit",
            ),
            (
                "progress",
                json!("not delivered"),
                "progress must explicitly say NOT DELIVERED or DELIVERED",
            ),
        ];
        for (field, value, message) in cases {
            let (mut roadmap, tasks) = fixture();
            roadmap["vectors"][0][field] = value;
            assert_eq!(errors(&roadmap, &tasks), [format!("VC-203-001 {message}")]);
        }
    }

    #[test]
    fn accepted_prefixes_priorities_and_jq_length_types_stay_accepted() {
        let (mut roadmap, tasks) = fixture();
        for priority in ["P0", "P1", "P2"] {
            for progress in ["DELIVERED", "DELIVERED suffix", "NOT DELIVERED suffix"] {
                roadmap["vectors"][0]["priority"] = json!(priority);
                roadmap["vectors"][0]["progress"] = json!(progress);
                roadmap["vectors"][0]["type"] = json!(-1);
                roadmap["vectors"][0]["vector"] = json!([null]);
                roadmap["vectors"][0]["mastery_target"] = json!(-160);
                assert!(errors(&roadmap, &tasks).is_empty());
            }
        }
        assert_eq!(jq_length(&json!({"a": 1})).unwrap(), 1.0);
        assert_eq!(jq_length(&json!(false)).unwrap(), 0.0);
        assert!(jq_length(&json!(true)).is_err());
    }

    #[test]
    fn direct_terminal_fan_in_uses_a_set_but_requires_all_47() {
        let (mut roadmap, tasks) = fixture();
        let deps = roadmap["vectors"][47]["depends_on"].as_array_mut().unwrap();
        deps.reverse();
        deps.push(json!("VC-203-001"));
        assert!(errors(&roadmap, &tasks).is_empty());
        roadmap["vectors"][47]["depends_on"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        assert_eq!(
            errors(&roadmap, &tasks),
            ["VC-203-048 must depend directly on every preceding VC-203 vector"]
        );
    }

    #[test]
    fn legacy_last_vector_wins_extra_vectors_and_indirect_cycles_remain_allowed() {
        let (mut roadmap, tasks) = fixture();
        let duplicate = roadmap["vectors"][0].clone();
        roadmap["vectors"][0]["priority"] = json!("invalid");
        roadmap["vectors"].as_array_mut().unwrap().extend([
            duplicate,
            json!({"id": "OTHER"}),
            json!({"id": 123}),
            Value::Null,
        ]);
        roadmap["vectors"][1]["depends_on"] = json!(["VC-203-003", "OTHER"]);
        roadmap["vectors"][2]["depends_on"] = json!(["VC-203-002"]);
        assert!(errors(&roadmap, &tasks).is_empty());
    }

    #[test]
    fn requirement_scanning_preserves_regex_boundaries_and_exactly_one_rule() {
        for goal in [
            "Requirement GMCP-203-001-01",
            "prefix Requirement GMCP-203-999-Mtail",
            "Requirement GMCP-203-000-010",
        ] {
            assert_eq!(requirement_matches(goal), 1, "{goal}");
        }
        for goal in [
            "requirement GMCP-203-001-01",
            "Requirement GMCP-203-01-01",
            "Requirement GMCP-203-0001-01",
            "Requirement GMCP-203-001-07",
            "Requirement GMCP-203-\u{0660}\u{0660}\u{0661}-01",
        ] {
            assert_eq!(requirement_matches(goal), 0, "{goal}");
        }
        let (roadmap, mut tasks) = fixture();
        tasks[0]["goal"] = json!("Requirement GMCP-203-001-01 Requirement GMCP-203-001-01");
        assert_eq!(
            errors(&roadmap, &tasks),
            ["expected 336 unique requirement tasks, found 335"]
        );
        tasks[0]["goal"] = tasks[1]["goal"].clone();
        assert!(errors(&roadmap, &tasks).is_empty());
        tasks.push(tasks[0].clone());
        assert_eq!(
            errors(&roadmap, &tasks),
            ["expected 336 unique requirement tasks, found 337"]
        );
    }

    #[test]
    fn unused_task_metadata_is_not_new_validation_but_jq_type_errors_fail() {
        let (mut roadmap, mut tasks) = fixture();
        tasks[0]["accept"] = json!({"cmd": "not an argv"});
        tasks[0]["roadmap"] = json!(false);
        tasks[0]["deps"] = json!(123);
        tasks.extend([Value::Null, json!({"goal": false})]);
        assert!(errors(&roadmap, &tasks).is_empty());
        tasks[0]["accept"] = json!(true);
        assert!(validate_values(&roadmap, &tasks).is_err());
        tasks[0]["accept"] = Value::Null;
        tasks[0]["goal"] = json!(123);
        assert!(validate_values(&roadmap, &tasks).is_err());
        tasks[0]["goal"] = Value::Null;
        roadmap["vectors"][0]["progress"] = json!(123);
        assert!(validate_values(&roadmap, &tasks).is_err());
        roadmap["vectors"][0]["progress"] = json!("DELIVERED");
        roadmap["vectors"][0]["depends_on"] = json!([123]);
        assert!(validate_values(&roadmap, &tasks).is_err());
    }

    struct FixtureDirectory(PathBuf);
    impl FixtureDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "gmcp-roadmap-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            for directory in [".agents/tasks/done", "docs"] {
                fs::create_dir_all(path.join(directory)).unwrap();
            }
            let (roadmap, tasks) = fixture();
            fs::write(
                path.join(".agents/roadmap.json"),
                serde_json::to_vec(&roadmap).unwrap(),
            )
            .unwrap();
            for (relative, tasks) in [
                (".agents/tasks/open.json", &tasks[..168]),
                (".agents/tasks/done/closed.json", &tasks[168..]),
            ] {
                let text = tasks
                    .iter()
                    .map(|task| format!("{task}\r\n"))
                    .collect::<String>();
                fs::write(path.join(relative), text).unwrap();
            }
            let doc = (1..=48)
                .map(vector_id)
                .chain(DOC_TOKENS.map(str::to_owned))
                .collect::<Vec<_>>()
                .join("\r\n");
            fs::write(path.join("docs/gmcp-roadmap.md"), doc).unwrap();
            fs::write(path.join("README.md"), "docs/gmcp-roadmap.md").unwrap();
            Self(path)
        }
    }
    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn filesystem_fixture_combines_open_done_streams_and_ignores_nested_tasks() {
        let root = FixtureDirectory::new();
        fs::create_dir(root.0.join(".agents/tasks/nested")).unwrap();
        fs::write(
            root.0.join(".agents/tasks/nested/ignored.json"),
            "malformed",
        )
        .unwrap();
        fs::write(root.0.join(".agents/tasks/ignored.txt"), "malformed").unwrap();
        fs::create_dir(root.0.join(".agents/roadmap-verdicts")).unwrap();
        fs::write(
            root.0.join(".agents/roadmap-verdicts/ignored.json"),
            "malformed",
        )
        .unwrap();
        assert!(validate(&root.0).unwrap().is_empty());
        fs::remove_file(root.0.join(".agents/tasks/done/closed.json")).unwrap();
        assert_eq!(
            validate(&root.0).unwrap(),
            ["expected 336 unique requirement tasks, found 168"]
        );
    }

    #[test]
    fn every_document_token_and_vector_mention_is_required() {
        let root = FixtureDirectory::new();
        let path = root.0.join("docs/gmcp-roadmap.md");
        let original = fs::read_to_string(&path).unwrap();
        for token in (1..=48).map(vector_id).chain(DOC_TOKENS.map(str::to_owned)) {
            fs::write(&path, original.replace(&token, "")).unwrap();
            let errors = validate(&root.0).unwrap();
            assert_eq!(errors.len(), 1, "{token}: {errors:?}");
            assert!(errors[0].contains(&token));
        }
    }

    #[test]
    fn missing_files_bad_json_and_invalid_task_records_fail_closed() {
        let root = FixtureDirectory::new();
        fs::write(root.0.join(".agents/tasks/broken.json"), "{").unwrap();
        assert!(validate(&root.0).is_err());
        fs::write(root.0.join(".agents/tasks/broken.json"), "[]").unwrap();
        assert!(validate(&root.0).is_err());
        fs::write(root.0.join(".agents/tasks/broken.json"), "null\n").unwrap();
        assert!(validate(&root.0).unwrap().is_empty());
        fs::remove_file(root.0.join("README.md")).unwrap();
        assert_eq!(
            validate(&root.0).unwrap(),
            ["README.md does not link the GMCP completion roadmap"]
        );
        fs::remove_file(root.0.join("docs/gmcp-roadmap.md")).unwrap();
        assert_eq!(validate(&root.0).unwrap().len(), 2);
        fs::write(root.0.join(".agents/roadmap.json"), "not json").unwrap();
        assert!(validate(&root.0).is_err());
        fs::remove_file(root.0.join(".agents/roadmap.json")).unwrap();
        assert_eq!(validate(&root.0).unwrap(), [".agents/roadmap.json missing"]);
    }

    #[test]
    fn hidden_json_matches_legacy_glob_then_find_behavior() {
        let root = FixtureDirectory::new();
        fs::write(root.0.join(".agents/tasks/.hidden.json"), "invalid").unwrap();
        assert!(read_tasks(&root.0).is_err());
        fs::remove_file(root.0.join(".agents/tasks/open.json")).unwrap();
        assert_eq!(read_tasks(&root.0).unwrap().len(), 168);
        fs::write(root.0.join(".agents/tasks/open.json"), "\n").unwrap();
        fs::remove_file(root.0.join(".agents/tasks/.hidden.json")).unwrap();
        fs::write(root.0.join(".agents/tasks/.json"), "invalid").unwrap();
        assert!(read_tasks(&root.0).is_err());
        fs::write(root.0.join(".agents/tasks/.json"), "{}\n").unwrap();
        assert_eq!(read_tasks(&root.0).unwrap().len(), 169);
    }

    #[test]
    fn empty_task_files_add_no_records_and_empty_vectors_fail() {
        let root = FixtureDirectory::new();
        fs::write(root.0.join(".agents/tasks/empty.json"), " \r\n\t").unwrap();
        assert!(validate(&root.0).unwrap().is_empty());
        fs::write(root.0.join(".agents/roadmap.json"), "{\"vectors\":[]}").unwrap();
        let errors = validate(&root.0).unwrap();
        assert_eq!(errors.len(), 49);
        assert_eq!(errors[0], "missing VC-203 vector: VC-203-001");
        assert_eq!(errors[47], "missing VC-203 vector: VC-203-048");
    }
}
