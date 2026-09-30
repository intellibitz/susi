//! Durable quorum commit boundary (T-DEVIN-11).
//!
//! A quorum decision is only reported as committed once the sealed record
//! is durably appended to the local commit ledger. Leader election, sealing,
//! append, and voter replication are injected seams so tests can force each
//! failure mode without a cluster key or network.
//!
//! Acknowledgement guarantees:
//! - Local durability is REQUIRED: no durable append → no commit report.
//! - Voter replication is AT-LEAST-ONCE best-effort: acks are counted and
//!   reported, but a missed voter is a recovery gap, not a commit veto — the
//!   sealed record carries the electorate and tally for later anti-entropy.

use crate::susi_core::commit_log::CommitRecord;
use crate::susi_error::EaiError;

/// What the commit boundary decided about the quorum value.
#[derive(Debug)]
pub enum CommitDurability {
    /// Sealed and durably appended to the commit ledger; `acked`/`voters`
    /// report how many voter replicas acknowledged the pushed record.
    Durable {
        record: Box<CommitRecord>,
        acked: usize,
        voters: usize,
    },
    /// No commit: leader election failed, sealing failed, or the ledger
    /// append did not durably succeed. The string is the failure detail —
    /// callers must surface it, never log-and-continue.
    Undurable(String),
}

impl CommitDurability {
    /// True only when the decision is durably committed locally.
    #[must_use]
    pub fn is_committed(&self) -> bool {
        matches!(self, CommitDurability::Durable { .. })
    }
}

/// Run the commit boundary for a quorum decision.
///
/// Order is strict: elect-and-seal → durable append → replicate to voters.
/// An append failure returns `Undurable` and no replication is attempted —
/// pushing a record that never landed locally would fork the ledger chain.
pub fn durable_commit<Seal, Append, Replicate>(
    seal: Seal,
    append: Append,
    voter_addrs: &[String],
    replicate: Replicate,
) -> CommitDurability
where
    Seal: FnOnce() -> Option<CommitRecord>,
    Append: FnOnce(&CommitRecord) -> crate::susi_error::EaiResult<()>,
    Replicate: Fn(&str, &CommitRecord) -> bool,
{
    let Some(record) = seal() else {
        return CommitDurability::Undurable(
            "leader election or record seal failed — no signed commit record".to_string(),
        );
    };
    if let Err(e) = append(&record) {
        return CommitDurability::Undurable(format!("commit ledger append failed: {e}"));
    }
    let mut acked = 0usize;
    for addr in voter_addrs {
        if replicate(addr, &record) {
            acked += 1;
        }
    }
    CommitDurability::Durable {
        record: Box::new(record),
        acked,
        voters: voter_addrs.len(),
    }
}

/// Convenience wrapper producing the governance error for `Undurable`.
#[must_use]
pub fn undurable_err(detail: &str) -> EaiError {
    EaiError::governance(format!("quorum decision not durably committed: {detail}"))
}

#[cfg(test)]
// Test module: panic-path macros are the assertion mechanism here; the
// mandate exemption applies to test code only.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn record() -> CommitRecord {
        CommitRecord {
            epoch: "ep".to_string(),
            coordinator: "coord".to_string(),
            electorate: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            tally: 2,
            quorum_threshold: 2,
            value_hash: "h".to_string(),
            value: "v".to_string(),
            committed_at: 0,
            seq: 1,
            leader: "a".to_string(),
            term: 1,
            prev_epoch: String::new(),
            kind: String::new(),
            signature: "sig".to_string(),
            member_sig: String::new(),
            member_pubkey: String::new(),
            subject_sig: String::new(),
            endorsements: Vec::new(),
        }
    }

    #[test]
    fn t_devin_11_append_failure_is_not_a_commit() {
        let replicated = AtomicUsize::new(0);
        let voters = vec!["a:1".to_string()];
        let out = durable_commit(
            || Some(record()),
            |_| Err(EaiError::io("ledger disk full".to_string())),
            &voters,
            |_, _| {
                replicated.fetch_add(1, Ordering::SeqCst);
                true
            },
        );
        match out {
            CommitDurability::Undurable(detail) => {
                assert!(detail.contains("ledger disk full"), "{detail}");
            }
            CommitDurability::Durable { .. } => panic!("append failure must not commit"),
        }
        // No replication on append failure — pushing a record that never
        // landed locally would fork the ledger chain.
        assert_eq!(replicated.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn t_devin_11_seal_failure_is_not_a_commit() {
        let appended = AtomicUsize::new(0);
        let voters = vec!["a:1".to_string()];
        let out = durable_commit(
            || None,
            |_| {
                appended.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            &voters,
            |_, _| true,
        );
        assert!(!out.is_committed());
        assert_eq!(appended.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn t_devin_11_durable_commit_replicates_and_counts_acks() {
        let voters = vec!["a:1".to_string(), "b:2".to_string(), "c:3".to_string()];
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let out = durable_commit(
            || Some(record()),
            |rec| {
                assert_eq!(rec.tally, 2);
                Ok(())
            },
            &voters,
            |addr, _| {
                seen.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(addr.to_string());
                !addr.starts_with('c') // c is unreachable
            },
        );
        match out {
            CommitDurability::Durable { acked, voters, .. } => {
                assert_eq!((acked, voters), (2, 3));
            }
            CommitDurability::Undurable(d) => panic!("must be durable: {d}"),
        }
        let pushed = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(pushed, vec!["a:1", "b:2", "c:3"]);
    }

    #[test]
    fn t_devin_11_voter_misses_do_not_veto_commit() {
        let voters = vec!["a:1".to_string(), "b:2".to_string()];
        let out = durable_commit(|| Some(record()), |_| Ok(()), &voters, |_, _| false);
        match out {
            CommitDurability::Durable { acked, voters, .. } => {
                assert_eq!((acked, voters), (0, 2));
            }
            CommitDurability::Undurable(d) => panic!("local durability suffices: {d}"),
        }
        assert!(out.is_committed());
    }
}
