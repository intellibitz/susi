use super::*;
use crate::pricing::{ModelPrice, TokenUsage};

fn price() -> ModelPrice {
    ModelPrice {
        input_miss: MicroUsd::from_micros(150_000),
        input_hit: MicroUsd::from_micros(3_000),
        output: MicroUsd::from_micros(600_000),
    }
}

/// Eight writers, each appending its own rows to one ledger at once. The
/// file must end with every row, each one whole: the appends are single
/// writes, so no line is split or spliced with another writer's line.
#[test]
fn concurrent_appenders_never_interleave_ledger_rows() {
    let dir = std::env::temp_dir().join(format!("susi-ledger-concurrency-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.jsonl");
    let _ = std::fs::remove_file(&path);
    let writers = 8;
    let rows_each = 200;
    std::thread::scope(|scope| {
        for writer in 0..writers {
            let path = path.clone();
            scope.spawn(move || {
                for row in 0..rows_each {
                    let task = format!("w{writer}-r{row}");
                    append_entry(&path, &entry(&task, Outcome::Completed)).unwrap();
                }
            });
        }
    });
    let entries = read_entries(&path).expect("every line must decode");
    assert_eq!(entries.len(), writers * rows_each);
    let mut seen: Vec<&str> = entries.iter().map(|e| e.task.as_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), writers * rows_each, "no row lost or duplicated");
    let _ = std::fs::remove_dir_all(&dir);
}

fn entry(task: &str, outcome: Outcome) -> LedgerEntry {
    LedgerEntry::settle(Dispatch {
        mission: "VC-202-025".into(),
        task: task.into(),
        attempt: 1,
        model: "deepseek-flash".into(),
        provider: "deepseek".into(),
        usage: TokenUsage::new(3_000_000, 2_400_000, 200_000),
        price: price(),
        started_unix_ms: 1_000,
        ended_unix_ms: 2_500,
        outcome,
    })
}

/// A row whose cost is exactly `micros`: one million input tokens at that
/// per-million miss rate and no output.
fn charged(mission: &str, task: &str, micros: u64) -> LedgerEntry {
    LedgerEntry::settle(Dispatch {
        mission: mission.into(),
        task: task.into(),
        attempt: 1,
        model: "deepseek-flash".into(),
        provider: "deepseek".into(),
        usage: TokenUsage::new(1_000_000, 0, 0),
        price: ModelPrice {
            input_miss: MicroUsd::from_micros(micros),
            input_hit: MicroUsd::ZERO,
            output: MicroUsd::ZERO,
        },
        started_unix_ms: 0,
        ended_unix_ms: 1,
        outcome: Outcome::Completed,
    })
}

#[test]
fn settle_prices_the_dispatch_and_keeps_the_rate() {
    let settled = entry("T-HARNESSPHASE0-1", Outcome::Accepted);
    assert_eq!(settled.cost, MicroUsd::from_micros(217_200));
    assert_eq!(settled.price, price());
    assert_eq!(settled.latency_ms(), 1_500);
    assert!(!settled.cached);
}

#[test]
fn a_cached_row_costs_nothing_and_says_so() {
    let cached = entry("T-HARNESSPHASE0-1", Outcome::Accepted).as_cached();
    assert!(cached.cached);
    assert_eq!(cached.cost, MicroUsd::ZERO);
    assert_eq!(cached.usage, TokenUsage::new(3_000_000, 2_400_000, 200_000));
    let json = serde_json::to_string(&cached).unwrap();
    assert!(json.contains("\"cached\":true"));
}

#[test]
fn an_old_row_without_the_cached_field_reads_as_not_cached() {
    let mut value = serde_json::to_value(entry("T-1", Outcome::Accepted)).unwrap();
    value.as_object_mut().unwrap().remove("cached");
    let parsed: LedgerEntry = serde_json::from_value(value).unwrap();
    assert!(!parsed.cached);
}

#[test]
fn ledger_round_trips_as_json_lines() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let accepted = entry("T-1", Outcome::Accepted);
    let failed = entry(
        "T-2",
        Outcome::ModelFailed {
            class: FailureClass::Timeout,
        },
    );
    append_entry(&path, &accepted).unwrap();
    append_entry(&path, &failed).unwrap();

    let read = read_entries(&path).unwrap();
    assert_eq!(read, vec![accepted, failed]);
    assert_eq!(total_cost(&read), MicroUsd::from_micros(217_200 + 217_200));
}

#[test]
fn cost_for_picks_only_this_missions_role() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    append_entry(&path, &charged("live-1", "planner", 120)).unwrap();
    append_entry(&path, &charged("live-1", "planner", 80)).unwrap();
    append_entry(&path, &charged("live-1", "coder-1", 900)).unwrap();
    append_entry(&path, &charged("live-1", "reviewer", 40)).unwrap();
    append_entry(&path, &charged("other-1", "planner", 700)).unwrap();

    let entries = read_entries(&path).unwrap();
    // Both planner rows and no other role: 120 + 80.
    assert_eq!(
        cost_for(&entries, "live-1", "planner"),
        MicroUsd::from_micros(200)
    );
    // The same role in another mission is not ours.
    assert_eq!(
        cost_for(&entries, "other-1", "planner"),
        MicroUsd::from_micros(700)
    );
    // A role that made no call spent nothing.
    assert_eq!(cost_for(&entries, "live-1", "absent"), MicroUsd::ZERO);
    assert_eq!(
        spent_for(&path, "live-1", "reviewer"),
        MicroUsd::from_micros(40)
    );
    assert_eq!(
        spent_for(&path, "live-1", "absent"),
        MicroUsd::ZERO,
        "unknown role is zero"
    );
    assert_eq!(
        spent_for(&dir.path().join("absent.jsonl"), "live-1", "planner"),
        MicroUsd::ZERO,
        "a missing ledger spends nothing"
    );
}

#[test]
fn a_malformed_line_is_an_error_not_a_dropped_charge() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    std::fs::write(&path, "{\"mission\":\"m\"}\nnot json\n").unwrap();
    let err = read_entries(&path).unwrap_err();
    assert!(err.to_string().contains("line 1"));
}

#[test]
fn a_torn_line_costs_only_itself_in_the_lenient_reader() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let good = serde_json::to_string(&entry("T-1", Outcome::Completed)).unwrap();
    std::fs::write(
        &path,
        format!(
            "{good}
{{\"mission\":\"tor
{good}
"
        ),
    )
    .unwrap();
    let (entries, skipped) = read_entries_lenient(&path);
    assert_eq!(entries.len(), 2, "both whole rows still count");
    assert_eq!(skipped, 1);
    assert_eq!(total_cost(&entries), MicroUsd::from_micros(217_200 * 2));
    assert_eq!(
        read_entries_lenient(&dir.path().join("absent.jsonl")),
        (Vec::new(), 0)
    );
}

/// A real tear: a crash or a full disk cuts a rejected row's free-text
/// reason inside a multi-byte character, with no newline after it. The
/// file is then not UTF-8, which made the lenient reader see no ledger at
/// all, and the next row went onto the torn line and was lost with it.
#[test]
fn a_tear_inside_a_character_costs_only_the_torn_row() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    append_entry(&path, &entry("T-1", Outcome::Completed)).unwrap();
    let rejected = serde_json::to_string(&entry(
        "T-2",
        Outcome::Rejected {
            reason: "quality gate failed: “tests” red".into(),
        },
    ))
    .unwrap();
    let cut = rejected.find('“').unwrap() + 1;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&rejected.as_bytes()[..cut]).unwrap();
    drop(file);
    append_entry(&path, &entry("T-3", Outcome::Completed)).unwrap();

    let (entries, skipped) = read_entries_lenient(&path);
    let tasks: Vec<&str> = entries.iter().map(|e| e.task.as_str()).collect();
    assert_eq!(tasks, ["T-1", "T-3"], "the row after the tear still counts");
    assert_eq!(skipped, 1);
    assert_eq!(total_cost(&entries), MicroUsd::from_micros(217_200 * 2));
}

/// harness-mission reads each role's spend back with [`spent_for`], and
/// the supervisor each merged task's cost with
/// [`crate::lifecycle::task_cost`]. Both read the ledger strictly, so one
/// torn line, inside a character or not, made every later readback $0.
#[test]
fn a_tear_costs_only_the_torn_row_in_the_spend_readbacks() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    append_entry(&path, &charged("live-1", "planner", 120)).unwrap();
    let rejected = serde_json::to_string(&entry(
        "T-2",
        Outcome::Rejected {
            reason: "quality gate failed: “tests” red".into(),
        },
    ))
    .unwrap();
    let cut = rejected.find('“').unwrap() + 1;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&rejected.as_bytes()[..cut]).unwrap();
    drop(file);
    // The next row lands on the torn line.
    append_entry(&path, &charged("live-1", "planner", 80)).unwrap();
    append_entry(&path, &charged("live-1", "planner", 40)).unwrap();
    assert!(std::str::from_utf8(&std::fs::read(&path).unwrap()).is_err());

    assert_eq!(
        spent_for(&path, "live-1", "planner"),
        MicroUsd::from_micros(240)
    );
    assert_eq!(
        crate::lifecycle::task_cost(&path, "live-1", "planner"),
        MicroUsd::from_micros(240)
    );
}

/// An ASCII tear has no newline after it either, so the next append is
/// glued onto the fragment's line. That row is still read, by every
/// lenient reader, whatever row type it reads into.
#[test]
fn a_row_appended_onto_a_torn_line_still_counts() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let torn = r#"{"mission":"m","task":"torn","usage":{"input":1,"cache_hit":0"#;
    std::fs::write(&path, torn).unwrap();
    append_entry(&path, &entry("T-1", Outcome::Completed)).unwrap();
    append_entry(&path, &entry("T-2", Outcome::Completed)).unwrap();

    let (entries, skipped) = read_entries_lenient(&path);
    let tasks: Vec<&str> = entries.iter().map(|e| e.task.as_str()).collect();
    assert_eq!(tasks, ["T-1", "T-2"]);
    assert_eq!(skipped, 1, "only the fragment is lost");
    let (values, skipped) = read_rows_lenient::<serde_json::Value>(&path);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["task"], "T-1");
    assert_eq!(skipped, 1);
    // A line that is not a torn row yields nothing from its inner objects.
    std::fs::write(&path, "{\"outcome\":{\"unknown\":{\"x\":1}}}\n").unwrap();
    assert_eq!(read_entries_lenient(&path), (Vec::new(), 1));
}

#[test]
fn missing_file_is_an_error() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.jsonl");
    assert!(read_entries(&path).is_err());
}

#[test]
fn append_entry_self_heals_torn_trailing_line_with_leading_newline() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    std::fs::write(&path, r#"{"mission":"m","task":"torn-without-newline"#).unwrap();

    let fresh = entry("T-FRESH", Outcome::Completed);
    append_entry(&path, &fresh).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "new entry must be placed on a fresh line after a torn trailing fragment"
    );
    let decoded_second: LedgerEntry = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(decoded_second.task, "T-FRESH");

    let report = verify_integrity(&path);
    assert_eq!(report.valid_entries, 1);
    assert_eq!(report.corrupted_lines, 1);
    assert_eq!(report.total_cost, fresh.cost);
    assert_eq!(report.non_monotonic_timestamps, 0);
    assert!(!report.is_clean());
}

#[test]
fn verify_integrity_audits_clean_and_non_monotonic_ledgers() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");

    assert_eq!(verify_integrity(&path), LedgerIntegrityReport::default());
    assert!(verify_integrity(&path).is_clean());

    let mut first = entry("T-1", Outcome::Completed);
    first.started_unix_ms = 2_000;
    first.ended_unix_ms = 2_500;
    append_entry(&path, &first).unwrap();

    let clean_report = verify_integrity(&path);
    assert!(clean_report.is_clean());
    assert_eq!(clean_report.valid_entries, 1);

    // Regressed started_unix_ms (1_500 < 2_000) and inverted span (1_400 < 1_500).
    let mut regressed = entry("T-2", Outcome::Completed);
    regressed.started_unix_ms = 1_500;
    regressed.ended_unix_ms = 1_600;
    append_entry(&path, &regressed).unwrap();

    let mut inverted = entry("T-3", Outcome::Completed);
    inverted.started_unix_ms = 3_000;
    inverted.ended_unix_ms = 2_900;
    append_entry(&path, &inverted).unwrap();

    let report = verify_integrity(&path);
    assert_eq!(report.valid_entries, 3);
    assert_eq!(report.corrupted_lines, 0);
    assert_eq!(report.non_monotonic_timestamps, 2);
    assert!(!report.is_clean());
}
