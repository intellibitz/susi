use crate::anti_entropy::{
    bounded_sweep, load_book_from, save_book_to, CatchUpBook, CatchUpBounds,
};

fn ledger(n: usize) -> Vec<u64> {
    (0..n as u64).collect()
}

fn bounds() -> CatchUpBounds {
    CatchUpBounds {
        batch_size: 3,
        max_apply_per_sweep: 5,
        max_scan_per_sweep: 8,
    }
}

#[test]
fn vc_201_037_converges_in_bounded_batches() {
    let remote = ledger(10);
    let mut book = CatchUpBook::default();
    let mut applied = Vec::new();
    let mut remote_seen = Vec::new();
    // Sweep 1: scan bound (8) stops before the tip.
    let out = bounded_sweep(
        &mut book,
        "p1",
        bounds(),
        &mut |off, lim| remote[off.min(remote.len())..(off + lim).min(remote.len())].to_vec(),
        &mut |_| true,
        &mut |b| applied.extend_from_slice(b),
        &mut |r: &u64| remote_seen.push(*r),
    );
    assert!(!out.converged && out.needs_more);
    // Apply bound (5) hits after two batches of 3 → scanned=6 < scan bound.
    assert_eq!(out.scanned, 6, "apply rate bound held first");
    assert!(out.applied <= 5 + 3, "batch overshoot bounded by page size");
    // Sweep 2: resumes at the cursor, reaches the tip.
    let out = bounded_sweep(
        &mut book,
        "p1",
        bounds(),
        &mut |off, lim| remote[off.min(remote.len())..(off + lim).min(remote.len())].to_vec(),
        &mut |_| true,
        &mut |b| applied.extend_from_slice(b),
        &mut |r: &u64| remote_seen.push(*r),
    );
    assert!(out.converged && !out.needs_more);
    assert_eq!(applied.len(), 10, "full ledger eventually applied");
}

#[test]
fn vc_201_037_interrupted_sweep_resumes_from_cursor() {
    let remote = ledger(9);
    let mut book = CatchUpBook::default();
    let mut applied = Vec::new();
    // "Crash" after one batch: drop mid-sweep, cursor persisted at 3.
    let out = bounded_sweep(
        &mut book,
        "p1",
        CatchUpBounds {
            batch_size: 3,
            max_apply_per_sweep: 3,
            max_scan_per_sweep: 3,
        },
        &mut |off, lim| remote[off.min(remote.len())..(off + lim).min(remote.len())].to_vec(),
        &mut |_| true,
        &mut |b| applied.extend_from_slice(b),
        &mut |_| {},
    );
    assert!(!out.converged);
    assert_eq!(book.peers["p1"].offset, 3);
    // Resume: fetch must start at the saved offset — no re-fetch of 0..3.
    let mut first_fetch_at = None;
    let _ = bounded_sweep(
        &mut book,
        "p1",
        bounds(),
        &mut |off, lim| {
            first_fetch_at.get_or_insert(off);
            remote[off.min(remote.len())..(off + lim).min(remote.len())].to_vec()
        },
        &mut |_| true,
        &mut |b| applied.extend_from_slice(b),
        &mut |_| {},
    );
    assert_eq!(first_fetch_at, Some(3), "resumed from persisted cursor");
    // No record applied twice.
    let mut sorted = applied.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), applied.len());
}

#[test]
fn vc_201_037_rate_limit_starves_nothing_and_still_converges() {
    let remote = ledger(20);
    let mut book = CatchUpBook::default();
    let mut applied = Vec::new();
    // Rate limit 5/sweep → converged over multiple sweeps, never more.
    for _ in 0..10 {
        let before = applied.len();
        let out = bounded_sweep(
            &mut book,
            "p1",
            bounds(),
            &mut |off, lim| remote[off.min(remote.len())..(off + lim).min(remote.len())].to_vec(),
            &mut |_| true,
            &mut |b| applied.extend_from_slice(b),
            &mut |_| {},
        );
        let gained = applied.len() - before;
        assert!(gained <= 8, "per-sweep apply bounded by scan (batch 3 ×)");
        if out.converged {
            break;
        }
    }
    assert_eq!(applied.len(), 20);
}

#[test]
fn vc_201_037_admit_gate_filters_without_stalling_cursor() {
    let remote = ledger(6);
    let mut book = CatchUpBook::default();
    let mut applied = Vec::new();
    // Reject even-numbered records — scan continues past refusals.
    let out = bounded_sweep(
        &mut book,
        "p1",
        bounds(),
        &mut |off, lim| remote[off.min(remote.len())..(off + lim).min(remote.len())].to_vec(),
        &mut |r| r % 2 == 1,
        &mut |b| applied.extend_from_slice(b),
        &mut |_| {},
    );
    assert!(out.converged);
    assert_eq!(applied, vec![1, 3, 5]);
}

#[test]
fn vc_201_037_cursor_roundtrips_through_disk() {
    let dir = std::env::temp_dir().join(format!("vc37-{}", std::process::id()));
    let path = dir.join("book.json");
    let mut book = CatchUpBook::default();
    book.mark_batch("p1", 42, 7, false);
    book.mark_batch("p2", 3, 3, true);
    save_book_to(&path, &book);
    let loaded = load_book_from(&path);
    assert_eq!(
        loaded.resume_offset("p1"),
        42,
        "incomplete resumes mid-ledger"
    );
    assert_eq!(loaded.resume_offset("p2"), 0, "complete wraps to head");
    let _ = std::fs::remove_dir_all(&dir);
}
