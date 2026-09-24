#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Integration test for ledger compaction (`susi commits compact` /
//! `commit_log::compact`): fold to `commit_snapshot.json`, archive
//! pre-snapshot records, retain per-coordinator anchors, and keep the
//! compacted view identical to the uncompacted one — plus the
//! below-floor intake bounds (archived redelivery is a no-op,
//! divergent content at an archived seq is refused as equivocation).
//!
//! Runs against a hermetic HOME so the host's real ledger is never
//! touched. `commit_log::ENV_LOCK` serializes against any other test
//! that swaps XDG/HOME.

use std::path::PathBuf;
use susi_config::cluster_key;
use susi_core::commit_log::{self, CommitInput, CommitRecord};

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
            "susi_compact_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
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

fn decision(coordinator: &str, leader: &str, value: &str) -> CommitRecord {
    CommitRecord::seal(CommitInput {
        coordinator,
        leader,
        electorate: vec!["A".into(), "B".into()],
        tally: 2,
        quorum_threshold: 2,
        value,
    })
    .expect("seal must succeed while a cluster.key exists")
}

#[test]
fn compaction_preserves_the_consensus_view_and_bounds_intake() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xCCu8; 32])).unwrap();
    let self_id = cluster_key::wire_node_id();

    // Two coordinators interleave: self seals 5 decisions + 1 member
    // delta; a second coordinator seals 3 decisions (self-signed for
    // test simplicity — the verify path doesn't care who sealed, only
    // that signatures hold).
    let mut sealed = Vec::new();
    for i in 1..=5 {
        let r = decision(&self_id, &self_id, &format!("self-{i}"));
        commit_log::append(&r).unwrap();
        sealed.push(r);
    }
    let member = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_ADD,
        "node-m@10.0.0.9:9090",
        vec![self_id.clone()],
    )
    .expect("seal_member must succeed for a well-formed spec");
    commit_log::append(&member).unwrap();
    for i in 1..=3 {
        let r = decision("node-q", "node-q", &format!("peer-{i}"));
        commit_log::append(&r).unwrap();
        sealed.push(r);
    }
    assert_eq!(commit_log::load().len(), 9);
    let before = commit_log::replay();

    // Compact: live keeps one anchor per coordinator; everything else
    // archives; the snapshot carries the derived state.
    let archived = commit_log::compact().expect("compact must succeed");
    assert_eq!(archived, 7, "9 records minus 2 anchors");
    let live = commit_log::load();
    assert_eq!(live.len(), 2, "one anchor per coordinator");
    assert!(commit_log::archive_path().exists());
    let snap = commit_log::load_snapshot().expect("snapshot must exist");
    assert_eq!(snap.high_water.get(&self_id), Some(&6));
    assert_eq!(snap.high_water.get("node-q"), Some(&3));
    assert_eq!(
        snap.roster.get("node-m").map(String::as_str),
        Some("10.0.0.9:9090")
    );

    // The compacted replay derives the identical consensus view.
    let after = commit_log::replay();
    assert_eq!(after.term, before.term);
    assert_eq!(after.coordinators, before.coordinators);
    assert_eq!(after.roster, before.roster);
    assert_eq!(after.banned, before.banned);
    assert_eq!(after.decisions, before.decisions);
    assert_eq!(after.memberships, before.memberships);
    assert!(
        after.anomalies.is_empty(),
        "compaction must not fabricate anomalies: {:?}",
        after.anomalies
    );

    // A below-floor redelivery dedups against the archive — the live
    // file must not regrow.
    let archived_r2 = sealed[1].clone(); // self seq 2 — archived
    assert!(archived_r2.seq < 6);
    commit_log::append(&archived_r2).unwrap();
    assert_eq!(
        commit_log::load().len(),
        2,
        "archived redelivery is a no-op"
    );

    // Divergent content at an archived seq is refused as equivocation
    // — the signature check never even runs; the archive comparison
    // fires first.
    let mut forged = archived_r2.clone();
    forged.value = "tampered".into();
    assert!(commit_log::append(&forged).is_err());

    // Post-snapshot sealing picks up from the anchor: next seq is 7,
    // prev_epoch pins the anchor — chain linkage survives compaction.
    let next = decision(&self_id, &self_id, "post-snapshot");
    assert_eq!(next.seq, 7);
    assert_eq!(next.prev_epoch, snap_anchor_epoch(&self_id));
    commit_log::append(&next).unwrap();
    let grown = commit_log::replay();
    assert_eq!(grown.coordinators.get(&self_id), Some(&7));
    assert!(grown.anomalies.is_empty());

    // A gap above the floor is still detected honestly: append 8+9,
    // then drop 8's line (this node "never received" it) —
    // missing_seqs_floored reports just {8}, never the archived era.
    let r8 = decision(&self_id, &self_id, "gap-8");
    assert_eq!(r8.seq, 8);
    commit_log::append(&r8).unwrap();
    let r9 = decision(&self_id, &self_id, "gap-9");
    assert_eq!(r9.seq, 9);
    commit_log::append(&r9).unwrap();
    let kept: String = commit_log::load()
        .iter()
        .filter(|r| !(r.coordinator == self_id && r.seq == 8))
        .filter_map(|r| serde_json::to_string(r).ok())
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(commit_log::ledger_path(), kept).unwrap();
    let missing = commit_log::missing_seqs_floored(
        &commit_log::load(),
        &self_id,
        r9.seq,
        commit_log::snapshot_floor(&self_id),
    );
    assert_eq!(missing, vec![8], "floor bounds gap reports post-snapshot");
}

/// The epoch of the retained live anchor for `coordinator`.
fn snap_anchor_epoch(coordinator: &str) -> String {
    commit_log::load()
        .into_iter()
        .filter(|r| r.coordinator == coordinator)
        .max_by_key(|r| r.seq)
        .expect("anchor must exist")
        .epoch
}
