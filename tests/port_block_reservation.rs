#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Port blocks are reserved across processes, without waiting.
//!
//! Several agents run their gates on one machine at the same time. The
//! listener tests (`host_only_privacy`, `peer_channel_hub`) each start real
//! daemons on a block of host ports. `common::reserve_port_block` gives every
//! test process its own block machine-wide; this test proves it against a
//! second process, which is what another agent's gate is.
//!
//! The offsets used here are far above any block a daemon test asks for, and
//! nothing is bound: only the reservation is exercised.

mod common;

use std::process::Command;

const CHILD_ENV: &str = "SUSI_PORT_BLOCK_CHILD_CANDIDATES";

/// Re-run this binary's `child_reports_the_block_it_reserved` in a second
/// process with `candidates`, and return what it printed.
fn child_reserves(candidates: &[u16]) -> String {
    let list: Vec<String> = candidates.iter().map(u16::to_string).collect();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_reports_the_block_it_reserved",
            "--nocapture",
        ])
        .env(CHILD_ENV, list.join(","))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("CHILD_RESERVED="))
        .unwrap_or_else(|| panic!("child printed no result: {stdout}"))
        .to_owned()
}

/// Does nothing in a normal run; the parent test sets the variable.
#[test]
fn child_reports_the_block_it_reserved() {
    let Ok(list) = std::env::var(CHILD_ENV) else {
        return;
    };
    let candidates: Vec<u16> = list.split(',').map(|v| v.parse().unwrap()).collect();
    match common::reserve_port_block(candidates, |_| true) {
        Some(offset) => println!("CHILD_RESERVED={offset}"),
        None => println!("CHILD_RESERVED=none"),
    }
}

#[test]
fn a_second_process_skips_a_held_block_and_never_waits() {
    // Unique per run, so a concurrent run of this very test (another agent's
    // gate) cannot hold the block this one expects to take.
    let base = 50_000 + (std::process::id() % 50) as u16 * 3;
    let (first, second, third) = (base, base + 1, base + 2);

    assert_eq!(common::reserve_port_block([first], |_| true), Some(first));
    // Held by this process: another process cannot have it, and it returns
    // at once instead of waiting for it.
    let started = std::time::Instant::now();
    assert_eq!(child_reserves(&[first]), "none");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    // Offered a choice, it takes the next block.
    assert_eq!(child_reserves(&[first, second]), second.to_string());
    // The child has exited, so its block is free again: nothing goes stale.
    assert_eq!(child_reserves(&[second]), second.to_string());

    // A block that is reserved but not actually free is not kept.
    assert_eq!(common::reserve_port_block([third], |_| false), None);
    assert_eq!(child_reserves(&[third]), third.to_string());
    // Within one process a block is handed out once.
    assert_eq!(
        common::reserve_port_block([first, second], |_| true),
        Some(second)
    );
    assert_eq!(common::reserve_port_block([first, second], |_| true), None);
}
