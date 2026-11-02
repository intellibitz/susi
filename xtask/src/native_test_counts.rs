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
    if !target_dir.is_absolute()
        || !cwd.is_absolute()
        || !target_dir.starts_with(cwd.join("target"))
    {
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
    let last_binary = *binaries
        .last()
        .ok_or("no outer Cargo test binaries executed")?;
    let mut boundaries = binaries;
    boundaries.extend(lines.iter().enumerate().filter_map(|(i, line)| {
        (i > last_binary && line.trim_start().starts_with("Doc-tests ")).then_some(i)
    }));
    let mut totals = (0_u64, 0_u64, 0_u64);
    for (index, start) in boundaries.iter().enumerate() {
        let end = boundaries.get(index + 1).copied().unwrap_or(lines.len());
        let suite = &lines[start + 1..end];
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
        for (total, value) in [
            (&mut totals.0, values[0]),
            (&mut totals.1, values[1]),
            (&mut totals.2, values[2]),
        ] {
            *total = total.checked_add(value).ok_or("test count overflow")?;
        }
    }
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;
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
