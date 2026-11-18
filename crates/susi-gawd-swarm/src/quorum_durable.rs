//! Durable quorum commit boundary (T-DEVIN-11).
//!
//! A quorum decision is only reported as committed once the sealed record
//! is durably appended to the local commit ledger. Leader election, sealing,
//! append, and voter replication are injected seams so tests can force each
//! failure mode without a cluster key or network.
//!
//! Acknowledgement guarantees:
//! - Local durability is REQUIRED: no durable append → no commit report.
//! - The primitive below keeps the original at-least-once recovery policy: acks
//!   are counted and reported, but a missed voter is not a veto.
//! - The supervisor uses [`durable_commit_requiring_acks`] because it promises
//!   a copy to every dispatched voter. A missing acknowledgement is therefore
//!   reported as uncommitted even though the local record remains recoverable.

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

/// Run a commit whose contract includes durable acknowledgement from every
/// dispatched voter.
///
/// `durable_commit` is intentionally retained for callers that promise only
/// local durability plus best-effort anti-entropy. The production quorum path
/// promises a replicated copy, so a partial acknowledgement must not be
/// labelled `QUORUM_COMMIT`. The local append is not rolled back: restart and
/// anti-entropy can still recover the record, while the returned status tells
/// the caller that the stronger commit contract was not met.
pub fn durable_commit_requiring_acks<Seal, Append, Replicate>(
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
    match durable_commit(seal, append, voter_addrs, replicate) {
        CommitDurability::Durable {
            record: _,
            acked,
            voters,
        } if acked < voters => CommitDurability::Undurable(format!(
            "commit ledger append succeeded but replication acknowledgements are incomplete: {acked}/{voters} voters acknowledged"
        )),
        CommitDurability::Durable {
            record,
            acked,
            voters,
        } => CommitDurability::Durable {
            record,
            acked,
            voters,
        },
        CommitDurability::Undurable(detail) => CommitDurability::Undurable(detail),
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

    #[test]
    fn swarm_gap_durable_quorum_lost_replication_ack_is_uncommitted() {
        let voters = vec!["a:1".to_string(), "b:2".to_string()];
        let out = durable_commit_requiring_acks(
            || Some(record()),
            |_| Ok(()),
            &voters,
            |addr, _| addr == "a:1",
        );
        match out {
            CommitDurability::Undurable(detail) => {
                assert!(detail.contains("1/2"), "{detail}");
                assert!(detail.contains("acknowledgements"), "{detail}");
            }
            CommitDurability::Durable { .. } => {
                panic!("a promised replica acknowledgement cannot be missing")
            }
        }
    }

    #[test]
    fn swarm_gap_durable_quorum_unwritable_ledger_never_reports_commit() {
        let replicated = AtomicUsize::new(0);
        let voters = vec!["a:1".to_string()];
        let out = durable_commit_requiring_acks(
            || Some(record()),
            |_| Err(EaiError::io("ledger is unwritable".to_string())),
            &voters,
            |_, _| {
                replicated.fetch_add(1, Ordering::SeqCst);
                true
            },
        );
        assert!(!out.is_committed());
        assert_eq!(replicated.load(Ordering::SeqCst), 0);
        match out {
            CommitDurability::Undurable(detail) => assert!(detail.contains("unwritable")),
            CommitDurability::Durable { .. } => {
                panic!("an unwritable ledger cannot report a durable quorum")
            }
        }
    }

    #[test]
    fn swarm_gap_durable_quorum_local_record_survives_coordinator_crash_window() {
        let retained = std::sync::Arc::new(std::sync::Mutex::new(None));
        let retained_by_append = std::sync::Arc::clone(&retained);
        let voters = vec!["a:1".to_string()];
        let out = durable_commit_requiring_acks(
            || Some(record()),
            move |rec| {
                *retained_by_append.lock().unwrap_or_else(|e| e.into_inner()) = Some(rec.clone());
                Ok(())
            },
            &voters,
            |_, _| false,
        );
        assert!(!out.is_committed());
        let recovered = retained.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(recovered.map(|rec| rec.value), Some("v".to_string()));
    }
}
