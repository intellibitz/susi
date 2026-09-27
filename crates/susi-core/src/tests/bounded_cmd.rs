//! `bounded_cmd` tests — kept out of the canonical source so crates that
//! `#[path]`-mount it never compile or run them.

use crate::bounded_cmd::output_within;
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(unix)]
#[test]
fn captures_output_and_closes_stdin() {
    // `cat` echoes stdin; with stdin closed it exits at once with nothing.
    let out = output_within(
        Command::new("sh").args(["-c", "echo out; echo err >&2; cat"]),
        Duration::from_secs(10),
    )
    .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
}

#[cfg(unix)]
#[test]
fn overdue_children_are_killed() {
    let started = Instant::now();
    let err =
        output_within(Command::new("sleep").arg("30"), Duration::from_millis(150)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
}
