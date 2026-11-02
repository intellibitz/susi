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
    if argv.get(1).map(String::as_str) != Some("test") {
        return Ok((0, 0, 0));
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
    let text = std::str::from_utf8(log)?;
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
}
