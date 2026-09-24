#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Integration test for cluster-key rotation (`susi peers rekey`):
//! the full seal → stage → append → activate cycle plus the
//! prior-epoch intake bounds (`commit_log::append_to`'s epoch rule).
//!
//! Runs against a hermetic HOME so the host's real `cluster.key` is
//! never touched — mutating it would cut this machine off its own
//! cluster. `commit_log::ENV_LOCK` serializes against any other test
//! that swaps XDG/HOME (seal/verify resolve the key through env).

use std::path::PathBuf;
use susi_config::cluster_key;
use susi_core::commit_log::{self, CommitInput, CommitRecord, KeyEpoch};

/// Env-seamed HOME/XDG; restores prior values on drop.
struct HomeGuard {
    tmp: PathBuf,
    prev_home: Option<std::ffi::OsString>,
    prev_userprofile: Option<std::ffi::OsString>,
    prev_xdg: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new() -> Self {
        let tmp = std::env::temp_dir().join(format!(
            "susi_rekey_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        // Pre-create `.susi` so SusiDirs picks the deterministic legacy
        // path under the swapped HOME.
        std::fs::create_dir_all(tmp.join(".susi")).unwrap();
        let g = Self {
            prev_home: std::env::var_os("HOME"),
            prev_userprofile: std::env::var_os("USERPROFILE"),
            prev_xdg: std::env::var_os("XDG_CONFIG_HOME"),
            tmp,
        };
        std::env::set_var("HOME", &g.tmp);
        std::env::set_var("USERPROFILE", &g.tmp);
        std::env::set_var("XDG_CONFIG_HOME", g.tmp.join("xdg"));
        g
    }
    fn config(&self) -> PathBuf {
        self.tmp.join(".susi")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        match &self.prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn decision(value: &str) -> CommitRecord {
    CommitRecord::seal(CommitInput {
        coordinator: "node-c",
        leader: "node-c",
        electorate: vec!["A".into(), "B".into()],
        tally: 2,
        quorum_threshold: 2,
        value,
    })
    .expect("seal must succeed while a cluster.key exists")
}

/// Rewrite the ledger dropping `drop_seq` from `coordinator` — the
/// file-level way to model "this node never received that record"
/// while keeping every other record's chain links genuine (no
/// re-signing anywhere in this test).
fn drop_ledger_seq(coordinator: &str, drop_seq: u64) {
    let kept: Vec<CommitRecord> = commit_log::load()
        .into_iter()
        .filter(|r| !(r.coordinator == coordinator && r.seq == drop_seq))
        .collect();
    let text: String = kept
        .iter()
        .map(|r| serde_json::to_string(r).unwrap() + "\n")
        .collect();
    std::fs::write(commit_log::ledger_path(), text).unwrap();
}

#[test]
fn rekey_rotates_epoch_and_bounds_prior_epoch_appends() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    let key_a = [0xAAu8; 32];
    let key_b = [0xBBu8; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(key_a)).unwrap();

    // Pre-rotation chain r1..r4, all appended so each seal sees the
    // right chain head; r4 is held back (not yet delivered "to this
    // node" — simulate by removing it after sealing r4's successor
    // is not needed: we simply keep it out of the final ledger).
    let r1 = decision("d1");
    assert_eq!(r1.signature_epoch(), Some(KeyEpoch::Current));
    commit_log::append(&r1).unwrap();
    let r2 = decision("d2");
    assert_eq!(r2.seq, 2);
    commit_log::append(&r2).unwrap();
    let r3 = decision("d3");
    assert_eq!(r3.seq, 3);
    assert_eq!(r3.prev_epoch, r2.epoch);
    commit_log::append(&r3).unwrap();
    let r4 = decision("d4");
    assert_eq!(r4.seq, 4);
    assert_eq!(r4.prev_epoch, r3.epoch);
    // This node never received r2 (internal gap) nor r4 (tail gap).
    drop_ledger_seq("node-c", 2);
    drop_ledger_seq("node-c", 4);
    assert_eq!(
        commit_log::load().iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![1, 3]
    );

    // Stage key_b, seal + append the rekey record — the apply path
    // activates the staged key whose fingerprint matches the record.
    let fp_b = cluster_key::key_fingerprint(&key_b);
    assert!(cluster_key::stage_key_to(
        &key_b,
        &dir.join("cluster.key.next")
    ));
    let rekey = CommitRecord::seal_rekey(
        &cluster_key::wire_node_id(),
        "node-c",
        &fp_b,
        vec!["node-c".into()],
    )
    .expect("seal_rekey must succeed with a valid fingerprint");
    assert_eq!(rekey.signature_epoch(), Some(KeyEpoch::Current));
    assert_eq!(rekey.kind, commit_log::KIND_CLUSTER_REKEY);
    // The local node is always a known coordinator for its own records.
    assert!(commit_log::member_coordinator_known_at(&rekey, &dir));
    commit_log::append(&rekey).unwrap();

    // Rotation happened: staged → current, current → prev.
    assert_eq!(
        std::fs::read_to_string(dir.join("cluster.key"))
            .unwrap()
            .trim(),
        hex::encode(key_b)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("cluster.key.prev"))
            .unwrap()
            .trim(),
        hex::encode(key_a)
    );
    assert!(!dir.join("cluster.key.next").exists());

    // Epoch classification: pre-rotation records verify under Prev but
    // remain valid history (verify() accepts both epochs).
    assert_eq!(r1.signature_epoch(), Some(KeyEpoch::Prev));
    assert!(r1.verify());

    // Chain-pinned internal-gap fill: r2's successor r3 is held and
    // names r2's epoch — the one case a prior-epoch record may append.
    assert_eq!(r2.signature_epoch(), Some(KeyEpoch::Prev));
    commit_log::append(&r2).unwrap();

    // Prior-epoch tail gap: r4 is genuine pre-rotation history but its
    // successor (seq 5) is not held — nothing pins it, so it refuses
    // exactly like revoked-key frontier traffic.
    assert_eq!(r4.signature_epoch(), Some(KeyEpoch::Prev));
    assert!(
        commit_log::append(&r4).is_err(),
        "prior-epoch record without a chain-pinned successor must refuse"
    );

    // Post-rotation records seal under the new key and append as
    // current-era traffic.
    let r5 = decision("d5");
    assert_eq!(r5.signature_epoch(), Some(KeyEpoch::Current));
    commit_log::append(&r5).unwrap();

    // Idempotent redelivery: the rekey record re-arriving post-rotation
    // verifies under Prev but dedups to a no-op, not a refusal.
    assert_eq!(rekey.signature_epoch(), Some(KeyEpoch::Prev));
    commit_log::append(&rekey).unwrap();

    // Rekey records need coordinator authority like member deltas — a
    // foreign coordinator's rotation record is not privileged.
    let foreign = CommitRecord::seal_rekey("node-x", "node-x", &fp_b, vec!["node-x".into()])
        .expect("seal_rekey must succeed with a valid fingerprint");
    assert!(!commit_log::member_coordinator_known_at(&foreign, &dir));
}
