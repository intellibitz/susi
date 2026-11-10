//! Print closed task IDs not yet published on the selected remote's main tree.
//! Native replacement for the legacy shell scanner; Git is the only subprocess.
#![forbid(unsafe_code)]

use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, ExitCode, Output, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn checked(output: Output) -> Result<Vec<u8>> {
    // for-each-ref can omit a broken ref with only a warning and exit zero.
    // These quiet plumbing queries must not hide lost receipt state.
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(format!(
            "git failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(output.stdout)
}

fn git(args: &[&str]) -> Result<Vec<u8>> {
    checked(Command::new("git").args(args).output()?)
}

fn selected_object<'a>(listing: &'a str, reference: &str) -> Result<Option<&'a str>> {
    let mut selected = None;
    for line in listing.lines() {
        let (name, object) = line.split_once(' ').ok_or("invalid remote-ref listing")?;
        if !matches!(object.len(), 40 | 64)
            || !object
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || (name != reference && !name.starts_with(&format!("{reference}/")))
        {
            return Err("invalid remote-ref listing".into());
        }
        if name == reference && selected.replace(object).is_some() {
            return Err("ambiguous remote-ref listing".into());
        }
    }
    Ok(selected)
}

fn published_paths(remote: &str) -> Result<BTreeSet<Vec<u8>>> {
    let reference = format!("refs/remotes/{remote}/main");
    git(&["check-ref-format", &reference])?;
    // show-ref --quiet conflates a broken loose ref with an absent one.
    // for-each-ref diagnoses corruption; select the exact name, not children.
    let listing = String::from_utf8(git(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        &reference,
    ])?)?;
    let Some(object) = selected_object(&listing, &reference)? else {
        return Ok(BTreeSet::new());
    };
    let commit = String::from_utf8(git(&[
        "rev-parse",
        "--verify",
        &format!("{object}^{{commit}}"),
    ])?)?;
    // NUL-separated names preserve quoting and nested task IDs. Include trees,
    // matching the old cat-file -e existence check without one process per ID.
    Ok(git(&[
        "ls-tree",
        "--full-tree",
        "-r",
        "-t",
        "-z",
        "--name-only",
        commit.trim(),
        "--",
        ".agents/tasks/done/",
    ])?
    .split(|byte| *byte == 0)
    .filter(|path| !path.is_empty())
    .map(<[u8]>::to_vec)
    .collect())
}

fn batch(objects: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("missing Git batch stdin")?;
    let input = objects.join("\n") + "\n";
    // Drain output while writing requests, so a large receipt batch cannot
    // deadlock when either OS pipe fills. No shell parsing or temporary files.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output();
    writer.join().map_err(|_| "Git batch writer panicked")??;
    checked(output?)
}

fn receipt<'a>(data: &mut &'a [u8], object: &str) -> Result<&'a [u8]> {
    let newline = data
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("truncated Git batch header")?;
    let header = std::str::from_utf8(&data[..newline])?;
    let fields: Vec<_> = header.split(' ').collect();
    if fields.len() != 3 || fields[0] != object || fields[1] != "blob" {
        return Err(format!("invalid receipt batch header: {header}").into());
    }
    let size = fields[2].parse::<usize>()?;
    let body = &data[newline + 1..];
    if body.get(size) != Some(&b'\n') {
        return Err("truncated Git batch body or missing separator".into());
    }
    *data = &body[size + 1..];
    Ok(&body[..size])
}

fn owed(agent: &str, remote: &str) -> Result<BTreeSet<String>> {
    let refs = String::from_utf8(git(&[
        "for-each-ref",
        "--sort=refname",
        "--format=%(refname:strip=2) %(objectname)",
        "refs/closed/",
    ])?)?;
    let refs: Vec<_> = refs
        .lines()
        .map(|line| line.split_once(' ').ok_or("invalid closed-ref listing"))
        .collect::<std::result::Result<_, _>>()?;
    let published = published_paths(remote)?;
    if refs.is_empty() {
        return Ok(BTreeSet::new());
    }
    let objects: Vec<_> = refs.iter().map(|(_, object)| *object).collect();
    let bytes = batch(&objects)?;
    let mut remaining = bytes.as_slice();
    let mut owed = BTreeSet::new();
    for (id, object) in refs {
        let body = receipt(&mut remaining, object)?;
        // Invalid JSON is not a receipt. Frame it by byte length first so it
        // cannot shift the agent or task ID belonging to the following blob.
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            continue;
        };
        if value.get("agent").and_then(Value::as_str) == Some(agent)
            && !published.contains(format!(".agents/tasks/done/{id}.json").as_bytes())
        {
            owed.insert(id.to_owned());
        }
    }
    if !remaining.is_empty() {
        return Err("unexpected trailing Git batch output".into());
    }
    Ok(owed)
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 || args[0].is_empty() {
        return Err("usage: owed-task-receipts <agent> [remote]".into());
    }
    let remote = args
        .get(1)
        .filter(|s| !s.is_empty())
        .map_or("origin", String::as_str);
    // Compute everything before printing: a failed scan never emits a partial
    // success-looking list or silently reports no merge debt.
    let ids = owed(&args[0], remote)?;
    let mut stdout = std::io::stdout().lock();
    for id in ids {
        writeln!(stdout, "{id}")?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("owed-task-receipts: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions.
mod tests {
    use super::{receipt, selected_object};

    #[test]
    fn remote_listing_requires_one_exact_ref_and_well_formed_objects() {
        let reference = "refs/remotes/origin/main";
        let object = "a".repeat(40);
        assert_eq!(selected_object("", reference).unwrap(), None);
        assert_eq!(
            selected_object(&format!("{reference}/child {object}\n"), reference).unwrap(),
            None
        );
        let exact = format!("{reference} {object}\n");
        assert_eq!(
            selected_object(&exact, reference).unwrap(),
            Some(object.as_str())
        );
        for malformed in [
            format!("{exact}{exact}"),
            format!("{reference} nope\n"),
            "broken\n".into(),
            format!("refs/remotes/other/main {object}\n"),
        ] {
            assert!(selected_object(&malformed, reference).is_err());
        }
    }

    #[test]
    fn batch_framing_uses_bytes_and_preserves_the_next_receipt() {
        let body = "{\n  \"agent\": \"\u{00e9}\"\n}\n";
        let bytes = format!("first blob {}\n{body}\nsecond blob 2\n{{}}\n", body.len());
        let mut data = bytes.as_bytes();
        assert_eq!(receipt(&mut data, "first").unwrap(), body.as_bytes());
        assert_eq!(receipt(&mut data, "second").unwrap(), b"{}");
        assert!(data.is_empty());
    }

    #[test]
    fn batch_framing_rejects_invalid_headers_and_truncated_bodies() {
        for data in [
            "first missing\n",
            "other blob 2\n{}\n",
            "first commit 2\n{}\n",
            "first blob invalid\n{}\n",
            "first blob 2\n{}",
            "first blob 3\n{}\n",
            "first blob 2\n{}x",
            "first blob 0",
        ] {
            assert!(receipt(&mut data.as_bytes(), "first").is_err(), "{data:?}");
        }
    }
}
