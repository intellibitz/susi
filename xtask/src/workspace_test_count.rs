//! Count completed, passing libtest cases across a Cargo invocation's harnesses.
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn passed_in_summary(line: &str) -> Option<u64> {
    let mut fields = line.strip_prefix("test result: ok. ")?.split("; ");
    let mut count = |unit| {
        let digits = fields.next()?.strip_suffix(unit)?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u64>().ok()
    };
    let passed = count(" passed")?;
    if count(" failed")? != 0 {
        return None;
    }
    count(" ignored")?;
    count(" measured")?;
    count(" filtered out")?;
    let elapsed = fields
        .next()?
        .strip_prefix("finished in ")?
        .strip_suffix('s')?;
    if !elapsed.parse::<f64>().ok()?.is_finite() || fields.next().is_some() {
        return None;
    }
    Some(passed)
}

pub(crate) fn verified_pass_count(path: &Path) -> io::Result<u64> {
    let reader = BufReader::new(File::open(path)?);
    let mut total = 0u64;
    for line in reader.split(b'\n') {
        let line = line?;
        if !line.starts_with(b"test result:") {
            continue;
        }
        // Negative regression fixtures run deliberately failing child harnesses.
        // Their output may also interleave. Only whole positive summaries
        // contribute evidence; the caller separately requires Cargo success.
        let Some(passed) = std::str::from_utf8(&line)
            .ok()
            .and_then(|line| passed_in_summary(line.trim_end()))
        else {
            continue;
        };
        total = total
            .checked_add(passed)
            .ok_or_else(|| invalid("Rust test count overflow"))?;
    }
    if total == 0 {
        return Err(invalid("workspace gate executed no passing Rust tests"));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn valid_and_invalid_summaries() {
        let summary = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s";
        assert_eq!(passed_in_summary(summary), Some(1));
        for invalid in [
            summary.replace("0 failed", "1 failed"),
            summary.replace("1 passed", "-1 passed"),
            summary.replace(" measured", " unmeasured"),
            String::from("test result: ok. 1 passed"),
        ] {
            assert_eq!(passed_in_summary(&invalid), None);
        }
    }

    #[test]
    fn real_harness_rejects_filtered_and_counts_executed_tests() {
        let exe = std::env::current_exe().unwrap();
        let path = std::env::temp_dir().join(format!("susi-test-count-{}.log", std::process::id()));
        for (name, expected) in [
            ("workspace_test_count::tests::no_such_test", None),
            (
                "workspace_test_count::tests::valid_and_invalid_summaries",
                Some(1),
            ),
        ] {
            let output = Command::new(&exe).args(["--exact", name]).output().unwrap();
            assert!(output.status.success());
            std::fs::write(&path, output.stdout).unwrap();
            assert_eq!(verified_pass_count(&path).ok(), expected);
        }
        std::fs::remove_file(path).unwrap();
    }
}
