//! Bounded anti-entropy catch-up for ledger repair (VC-201-037).
//!
//! `sync_commit_ledger_from` pulls a peer's commit ledger in bounded
//! batches instead of retaining the whole remote set, applies at most
//! `rate_limit` records per sweep so a deep backlog cannot starve
//! foreground missions, and persists a per-peer cursor after every batch
//! so an interrupted sweep resumes mid-ledger instead of restarting from
//! zero. Convergence is preserved: each sweep advances the cursor until
//! the remote tip.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Durable per-peer resume point. Keyed by peer address inside
/// `CatchUpBook`; persisted as JSON so a crash mid-sweep loses at most
/// one batch of work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatchUpCursor {
    /// Remote offset the next fetch should start at.
    pub offset: usize,
    /// Total records applied over the cursor's lifetime (audit).
    pub applied: u64,
    /// Whether the previous sweep reached the remote tip — a complete
    /// cursor resets to 0 so the next sweep re-scans from the head
    /// (remote set may have shrunk or diverged).
    pub complete: bool,
}

/// Per-peer cursors, one file per store — mergeable like the rest of
/// `.agents` state (one peer's progress never clobbers another's).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CatchUpBook {
    pub peers: BTreeMap<String, CatchUpCursor>,
}

impl CatchUpBook {
    /// Cursor to resume `peer` from: a completed previous run restarts
    /// at 0; an interrupted one continues from its saved offset.
    #[must_use]
    pub fn resume_offset(&self, peer: &str) -> usize {
        match self.peers.get(peer) {
            Some(c) if !c.complete => c.offset,
            _ => 0,
        }
    }

    /// Record a batch landing for `peer`.
    pub fn mark_batch(&mut self, peer: &str, offset: usize, applied: u64, tip: bool) {
        let c = self.peers.entry(peer.to_string()).or_default();
        c.offset = offset;
        c.applied += applied;
        c.complete = tip;
    }
}

/// Where per-peer cursors persist — next to the ledger they pace.
#[must_use]
pub fn book_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("commit_catchup.json")
}

/// Load cursors; absent or corrupt → empty book (a missed cursor only
/// costs a re-scan, never a wrong offset).
#[must_use]
pub fn load_book() -> CatchUpBook {
    load_book_from(&book_path())
}

/// Path-seamed loader for tests.
#[must_use]
pub fn load_book_from(path: &std::path::Path) -> CatchUpBook {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist cursors after a sweep. Best-effort: a write failure just
/// costs the next sweep a re-fetch of already-deduped records.
pub fn save_book(book: &CatchUpBook) {
    save_book_to(&book_path(), book);
}

/// Path-seamed saver for tests.
pub fn save_book_to(path: &std::path::Path, book: &CatchUpBook) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(book) {
        let _ = std::fs::write(path, json);
    }
}

/// Bounds one catch-up sweep.
#[derive(Debug, Clone, Copy)]
pub struct CatchUpBounds {
    /// Records fetched per RPC page.
    pub batch_size: usize,
    /// Records applied per sweep — the rate limit. Beyond this the
    /// sweep yields to foreground work and resumes next cycle.
    pub max_apply_per_sweep: usize,
    /// Remote records scanned per sweep — bounds `their_keys` memory
    /// even when the remote ledger is huge.
    pub max_scan_per_sweep: usize,
}

impl Default for CatchUpBounds {
    fn default() -> Self {
        Self {
            batch_size: 256,
            max_apply_per_sweep: 512,
            max_scan_per_sweep: 4096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Remote records scanned this sweep.
    pub scanned: usize,
    /// Records passed to `apply` this sweep.
    pub applied: usize,
    /// Remote tip reached — peer is caught up.
    pub converged: bool,
    /// True when the sweep stopped on a bound rather than the tip;
    /// the peer needs another sweep to converge.
    pub needs_more: bool,
}

/// One bounded anti-entropy sweep for a lagging peer.
///
/// - `fetch(offset, limit) -> Vec<R>` pages the remote ledger; a short
///   or empty page marks the tip.
/// - `admit(&R) -> bool` is the caller's verify+dedup+authority gate —
///   rejected records are scanned but never applied.
/// - `apply(&[R])` lands one bounded batch; the cursor is persisted
///   *after* it returns, so a crash can re-drive at most one batch.
///
/// Scanned keys are fed to `note_remote` so the caller accumulates the
/// peer's held set for symmetric repair — bounded by
/// `max_scan_per_sweep` because the sweep stops there.
#[allow(clippy::too_many_arguments)] // sweep driver — each callback is a distinct seam
pub fn bounded_sweep<R: Clone>(
    book: &mut CatchUpBook,
    peer: &str,
    bounds: CatchUpBounds,
    fetch: &mut dyn FnMut(usize, usize) -> Vec<R>,
    admit: &mut dyn FnMut(&R) -> bool,
    apply: &mut dyn FnMut(&[R]),
    note_remote: &mut dyn FnMut(&R),
) -> SweepOutcome {
    let mut offset = book.resume_offset(peer);
    let mut scanned = 0usize;
    let mut applied = 0usize;
    loop {
        let page = fetch(offset, bounds.batch_size);
        if page.is_empty() {
            book.mark_batch(peer, offset, 0, true);
            return SweepOutcome {
                scanned,
                applied,
                converged: true,
                needs_more: false,
            };
        }
        let page_len = page.len();
        offset += page_len;
        let mut batch: Vec<R> = Vec::new();
        for r in &page {
            note_remote(r);
            if admit(r) {
                batch.push(r.clone());
            }
        }
        scanned += page_len;
        if !batch.is_empty() {
            let n = batch.len() as u64;
            apply(&batch);
            applied += batch.len();
            book.mark_batch(peer, offset, n, false);
        } else {
            book.mark_batch(peer, offset, 0, false);
        }
        let tip = page_len < bounds.batch_size;
        let rate_hit = applied >= bounds.max_apply_per_sweep;
        let scan_hit = scanned >= bounds.max_scan_per_sweep;
        if tip || rate_hit || scan_hit {
            book.mark_batch(peer, offset, 0, tip);
            return SweepOutcome {
                scanned,
                applied,
                // The tip was seen — caught up even if a bound also hit.
                converged: tip,
                needs_more: !tip,
            };
        }
    }
}
