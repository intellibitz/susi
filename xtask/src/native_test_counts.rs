//! Count only the outer Cargo test harnesses of an identified native command.
use std::path::Path;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn option<'a>(argv: &'a [String], name: &str) -> Result<&'a str> {
    let matches: Vec<_> = argv.windows(2).filter(|pair| pair[0] == name).collect();
    if matches.len() != 1 {
        return Err(format!("expected exactly one {name}").into());
    }
    Ok(&matches[0][1])
}

/// Count outer Cargo suites using the actual command's target directory and
/// target triple. Fixture Cargo output inside a test is not another outer suite.
pub fn test_counts(log: &[u8], argv: &[String], cwd: &Path) -> Result<(u64, u64, u64)> {
    if argv.get(1).map(String::as_str) == Some("nextest") {
        return nextest_counts(&strip_ansi(std::str::from_utf8(log)?));
    }
    if argv.get(1).map(String::as_str) != Some("test") {
        return Ok((0, 0, 0));
    }
    if argv.iter().any(|arg| arg == "--doc") {
        return doctest_counts(&strip_ansi(std::str::from_utf8(log)?));
    }
    let target = option(argv, "--target")?;
    let target_dir = Path::new(option(argv, "--target-dir")?);
    let is_abs = |p: &Path| p.is_absolute() || p.to_string_lossy().starts_with('/');
    if !is_abs(target_dir) || !is_abs(cwd) || !target_dir.starts_with(cwd.join("target")) {
        return Err("test output directory is not bound to the isolated candidate".into());
    }
    let absolute = format!(
        "{}/{target}/",
        target_dir.to_string_lossy().replace('\\', "/")
    );
    let relative = format!(
        "{}/{target}/",
        target_dir
            .strip_prefix(cwd)?
            .to_string_lossy()
            .replace('\\', "/")
    );
    let text = strip_ansi(std::str::from_utf8(log)?);
    let lines: Vec<_> = text.lines().collect();
    let binaries: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let running = line.trim_start().strip_prefix("Running ")?;
            let (_, path) = running.rsplit_once(" (")?;
            let path = path.strip_suffix(')')?.replace('\\', "/");
            (path.starts_with(&absolute) || path.starts_with(&relative)).then_some(i)
        })
        .collect();
    if binaries.is_empty() {
        return Err("no outer Cargo test binaries executed".into());
    }
    let mut totals = (0_u64, 0_u64, 0_u64);
    for (index, start) in binaries.iter().enumerate() {
        let end = binaries.get(index + 1).copied().unwrap_or(lines.len());
        let suite = &lines[start + 1..end];
        if index + 1 != binaries.len() {
            add_counts(&mut totals, suite_counts(suite)?)?;
            continue;
        }
        // A fixture inside the final binary can print its own Doc-tests banner.
        // Admit a trailing Cargo doctest phase only when the preceding outer
        // binary is complete and every following doctest block has exactly one
        // announced harness and one final result. A nested doctest block also
        // contains the enclosing binary's result, so it cannot become a phase.
        let docs: Vec<_> = suite
            .iter()
            .enumerate()
            .filter_map(|(i, line)| line.trim_start().starts_with("Doc-tests ").then_some(i))
            .collect();
        let mut phase = None;
        for (candidate, split) in docs.iter().enumerate() {
            let Ok(mut counts) = suite_counts(&suite[..*split]) else {
                continue;
            };
            let mut valid = true;
            for (offset, start) in docs[candidate..].iter().enumerate() {
                let end = docs
                    .get(candidate + offset + 1)
                    .copied()
                    .unwrap_or(suite.len());
                let block = &suite[start + 1..end];
                if block
                    .iter()
                    .filter(|line| line.starts_with("running "))
                    .count()
                    != 1
                    || block
                        .iter()
                        .filter(|line| line.starts_with("test result: "))
                        .count()
                        != 1
                {
                    valid = false;
                    break;
                }
                match suite_counts(block) {
                    Ok(next) => add_counts(&mut counts, next)?,
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid {
                phase = Some(counts);
                break;
            }
        }
        let counts = if let Some(counts) = phase {
            counts
        } else {
            let last_result = suite
                .iter()
                .rposition(|line| line.starts_with("test result: "))
                .ok_or("outer suite omitted final result")?;
            if docs.iter().any(|header| *header > last_result) {
                return Err("incomplete trailing Cargo doctest phase".into());
            }
            if let Some(last_doc) = docs.last() {
                let tail = &suite[last_doc + 1..];
                let starts = tail
                    .iter()
                    .filter(|line| line.starts_with("running "))
                    .count();
                let results = tail
                    .iter()
                    .filter(|line| line.starts_with("test result: "))
                    .count();
                if results <= starts {
                    return Err("malformed or ambiguous trailing Cargo doctest phase".into());
                }
            }
            suite_counts(suite)?
        };
        add_counts(&mut totals, counts)?;
    }
    Ok(totals)
}

/// Counts from `cargo nextest run`'s one final `Summary [ <time>] <n> tests
/// run: <passed> passed[ (..)], <failed> failed, <skipped> skipped` line:
/// `(passed, failed, skipped)`. Exactly one summary must be present and the
/// run must be complete (`<n>/<total> tests run` means some never ran); any
/// unknown outcome word fails closed. Timed-out tests count as failed.
pub fn nextest_counts(text: &str) -> Result<(u64, u64, u64)> {
    let summaries: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("Summary ["))
        .collect();
    let [summary] = summaries.as_slice() else {
        return Err(format!("expected one nextest summary, found {}", summaries.len()).into());
    };
    let (_, body) = summary
        .split_once("] ")
        .ok_or("malformed nextest summary timing")?;
    let (run, outcomes) = body
        .split_once(" run: ")
        .ok_or("malformed nextest summary")?;
    let run = run
        .strip_suffix(" tests")
        .or_else(|| run.strip_suffix(" test"))
        .ok_or("malformed nextest run count")?;
    let run: u64 = run
        .parse()
        .map_err(|_| "incomplete nextest run (not every test ran)")?;
    let (mut passed, mut failed, mut skipped) = (0_u64, 0_u64, 0_u64);
    for item in outcomes.split(", ") {
        // `12 passed (3 slow)`: the parenthetical only annotates the count.
        let item = item.split(" (").next().unwrap_or(item).trim();
        let (count, word) = item.split_once(' ').ok_or("malformed nextest outcome")?;
        let count: u64 = count.parse().map_err(|_| "malformed nextest count")?;
        match word {
            "passed" => passed = count,
            "failed" | "timed out" | "exec failed" => {
                failed = failed.checked_add(count).ok_or("nextest count overflow")?;
            }
            "skipped" => skipped = count,
            other => return Err(format!("unknown nextest outcome: {other}").into()),
        }
    }
    if passed.checked_add(failed) != Some(run) {
        return Err("nextest summary counts do not add up".into());
    }
    Ok((passed, failed, skipped))
}

/// Counts from a `cargo test --doc` run: every `Doc-tests <crate>` block must
/// announce one harness and end in exactly one result. A run whose crates
/// have no doctests has zero blocks or zero-test blocks and counts zero.
pub fn doctest_counts(text: &str) -> Result<(u64, u64, u64)> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().any(|line| {
        line.trim_start()
            .strip_prefix("Running ")
            .is_some_and(|rest| rest.contains(" ("))
    }) {
        return Err("a doctest gate ran non-doctest harnesses".into());
    }
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| line.trim_start().starts_with("Doc-tests ").then_some(i))
        .collect();
    let mut totals = (0_u64, 0_u64, 0_u64);
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(lines.len());
        let block = &lines[start + 1..end];
        let results = block
            .iter()
            .filter(|line| line.starts_with("test result: "))
            .count();
        let harnesses = block
            .iter()
            .filter(|line| line.starts_with("running "))
            .count();
        if results != 1 || harnesses != 1 {
            return Err("malformed Cargo doctest block".into());
        }
        add_counts(&mut totals, suite_counts(block)?)?;
    }
    Ok(totals)
}

/// Remove ANSI CSI sequences: hosted CI sets `CARGO_TERM_COLOR=always`, and a
/// coloured `Running` or `test result:` line must parse like a plain one.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn add_counts(total: &mut (u64, u64, u64), next: (u64, u64, u64)) -> Result<()> {
    for (target, value) in [
        (&mut total.0, next.0),
        (&mut total.1, next.1),
        (&mut total.2, next.2),
    ] {
        *target = target.checked_add(value).ok_or("test count overflow")?;
    }
    Ok(())
}

fn suite_counts(suite: &[&str]) -> Result<(u64, u64, u64)> {
    let declared = suite
        .iter()
        .find_map(|line| {
            let rest = line.strip_prefix("running ")?;
            rest.strip_suffix(" tests")
                .or_else(|| rest.strip_suffix(" test"))
        })
        .ok_or("outer suite omitted announced test count")?
        .parse::<u64>()?;
    let result = suite
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix("test result: "))
        .ok_or("outer suite omitted final test result")?;
    let (outcome, counts) = result.split_once(". ").ok_or("malformed test summary")?;
    let fields: Vec<_> = counts.split(';').collect();
    if fields.len() < 5 {
        return Err("incomplete test summary".into());
    }
    let mut values = [0_u64; 5];
    for (i, expected) in ["passed", "failed", "ignored", "measured", "filtered out"]
        .iter()
        .enumerate()
    {
        let (count, label) = fields[i]
            .trim()
            .split_once(' ')
            .ok_or("malformed test count")?;
        if label != *expected {
            return Err("malformed test count label".into());
        }
        values[i] = count.parse()?;
    }
    let announced = values[..4]
        .iter()
        .try_fold(0_u64, |sum, v| sum.checked_add(*v))
        .ok_or("test count overflow")?;
    if announced != declared
        || !matches!(outcome, "ok" | "FAILED")
        || (outcome == "FAILED") != (values[1] != 0)
    {
        return Err("outer test announcement/result mismatch".into());
    }
    Ok((values[0], values[1], values[2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command() -> Vec<String> {
        [
            "cargo",
            "test",
            "--target",
            "host",
            "--target-dir",
            "/repo/target",
        ]
        .iter()
        .map(|s| (*s).into())
        .collect()
    }
    #[test]
    fn coloured_cargo_output_counts_like_plain_output() {
        let coloured = "\u{1b}[1m\u{1b}[92m     Running\u{1b}[0m unittests src/lib.rs (target/host/debug/deps/outer)\nrunning 1 test\ntest actual_test ... \u{1b}[32mok\u{1b}[0m\n\ntest result: \u{1b}[32mok\u{1b}[0m. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n";
        assert_eq!(
            test_counts(coloured.as_bytes(), &command(), Path::new("/repo")).unwrap(),
            (1, 0, 0)
        );
    }
    #[test]
    fn final_binary_nested_doctests_do_not_become_outer_suites() {
        let parent = "     Running unittests src/lib.rs (target/host/debug/deps/outer)\nrunning 1 test\n     Running unittests src/lib.rs (/tmp/fixture/target/debug/deps/inner)\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n   Doc-tests inner\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\ntest enclosing_fixture ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n";
        assert_eq!(
            test_counts(parent.as_bytes(), &command(), Path::new("/repo")).unwrap(),
            (1, 0, 0)
        );
        let with_docs = format!("{parent}   Doc-tests outer\nrunning 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n");
        assert_eq!(
            test_counts(with_docs.as_bytes(), &command(), Path::new("/repo")).unwrap(),
            (3, 0, 0)
        );
    }
    #[test]
    fn zero_and_failed_outer_suites_cannot_be_filled_by_incidental_results() {
        let zero = b"     Running unittests src/lib.rs (target/host/debug/deps/outer)\nrunning 0 tests\nchild: test result: ok. 99 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n";
        assert_eq!(
            test_counts(zero, &command(), Path::new("/repo")).unwrap(),
            (0, 0, 0)
        );
        let failed = b"     Running unittests src/lib.rs (target/host/debug/deps/outer)\nrunning 2 tests\ntest result: ok. 50 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;\n";
        assert_eq!(
            test_counts(failed, &command(), Path::new("/repo")).unwrap(),
            (1, 1, 0)
        );
        let incomplete = "     Running unittests src/lib.rs (target/host/debug/deps/outer)\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n   Doc-tests missing\n";
        assert!(test_counts(incomplete.as_bytes(), &command(), Path::new("/repo")).is_err());
        let malformed = "     Running unittests src/lib.rs (target/host/debug/deps/outer)\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n   Doc-tests outer\nrunning 2 tests\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n";
        assert!(test_counts(malformed.as_bytes(), &command(), Path::new("/repo")).is_err());
    }
    #[test]
    fn counts_require_actual_nonempty_results_and_reject_malformed_values() {
        let argv: Vec<String> = [
            "cargo",
            "test",
            "--target",
            "host",
            "--target-dir",
            "/repo/target",
        ]
        .iter()
        .map(|s| (*s).into())
        .collect();
        let log = b"     Running unittests src/lib.rs (target/host/debug/deps/suite)\nrunning 5 tests\n     Running unittests src/lib.rs (/tmp/fixture/target/debug/deps/nested)\nrunning 20 tests\ntest result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\ntest result: ok. 3 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out;\n   Doc-tests suite\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n";
        assert_eq!(
            test_counts(log, &argv, Path::new("/repo")).unwrap(),
            (3, 0, 2)
        );
        assert!(test_counts(b"running 3 tests\n", &argv, Path::new("/repo")).is_err());
        assert_eq!(
            test_counts(log, &["cargo".into(), "clippy".into()], Path::new("/repo")).unwrap(),
            (0, 0, 0)
        );
        let bad = String::from_utf8(log.to_vec())
            .unwrap()
            .replace("3 passed", "2 passed");
        assert!(test_counts(bad.as_bytes(), &argv, Path::new("/repo")).is_err());
    }
    #[test]
    fn nextest_summary_counts_complete_runs_and_fails_closed() {
        let log = "    Starting 5 tests across 2 binaries (1 test skipped)\n        PASS [   0.010s] a tests::x\n------------\n     Summary [   1.234s] 5 tests run: 5 passed (1 slow), 1 skipped\n";
        assert_eq!(nextest_counts(log).unwrap(), (5, 0, 1));
        let failed =
            "     Summary [   1.234s] 5 tests run: 3 passed, 1 failed, 1 timed out, 0 skipped\n";
        assert_eq!(nextest_counts(failed).unwrap(), (3, 2, 0));
        let one = "     Summary [   0.100s] 1 test run: 1 passed, 0 skipped\n";
        assert_eq!(nextest_counts(one).unwrap(), (1, 0, 0));
        for bad in [
            "",
            "     Summary [   1.0s] 3/10 tests run: 3 passed, 0 skipped\n",
            "     Summary [   1.0s] 5 tests run: 4 passed, 0 skipped\n",
            "     Summary [   1.0s] 2 tests run: 2 passed, 1 mystery\n",
            "     Summary [   1.0s] 1 test run: 1 passed, 0 skipped\n     Summary [   1.0s] 1 test run: 1 passed, 0 skipped\n",
        ] {
            assert!(nextest_counts(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn doctest_counts_sum_blocks_and_allow_crates_without_doctests() {
        let log = "   Doc-tests alpha\n\nrunning 2 tests\ntest src/lib.rs - f (line 3) ... ok\ntest src/lib.rs - g (line 9) ... ok\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.50s\n\n   Doc-tests beta\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
        assert_eq!(doctest_counts(log).unwrap(), (2, 0, 1));
        assert_eq!(doctest_counts("").unwrap(), (0, 0, 0));
        let broken = "   Doc-tests alpha\n\nrunning 1 test\n";
        assert!(doctest_counts(broken).is_err());
        let mixed = "     Running unittests src/lib.rs (target/x/debug/deps/a-1.exe)\n";
        assert!(doctest_counts(mixed).is_err());
    }
}
