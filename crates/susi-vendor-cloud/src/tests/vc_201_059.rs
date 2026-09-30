use crate::orphan_gc::{reconcile, Authoritative, Resource};
use std::collections::BTreeSet;

fn res(id: &str, mission: &str, deployment: &str, expires: u64) -> Resource {
    Resource {
        id: id.into(),
        owner: Some("susi".into()),
        mission: mission.into(),
        deployment: deployment.into(),
        retain: false,
        expires_unix: expires,
    }
}

#[test]
fn vc_201_059_deletes_only_expired_susi_owned_orphans() {
    let live = vec![
        // Orphaned (mission gone), expired → delete.
        res("r1", "m-dead", "d-dead", 50),
        // Live mission → keep even though expired.
        res("r2", "m-live", "", 50),
        // Orphaned but retained → keep.
        Resource {
            retain: true,
            ..res("r3", "m-dead", "", 50)
        },
        // Orphaned but not yet expired → keep.
        res("r4", "", "", 500),
        // Foreign (no owner tag) → never touched.
        Resource {
            owner: None,
            ..res("r5", "", "", 1)
        },
    ];
    let missions = BTreeSet::from(["m-live".to_string()]);
    let deployments = BTreeSet::new();
    let mut deleted = Vec::new();
    let auth = Authoritative {
        live_missions: &missions,
        live_deployments: &deployments,
        now_unix: 100,
        dry_run: false,
    };
    let report = reconcile(&live, &auth, &mut |id| {
        deleted.push(id.to_string());
    });
    assert_eq!(report.delete, vec!["r1"]);
    assert_eq!(deleted, vec!["r1"]);
    assert_eq!(report.keep, vec!["r2", "r3", "r4"]);
    assert_eq!(report.foreign, vec!["r5"]);
    assert!(!report.dry_run);
}

#[test]
fn vc_201_059_dry_run_reports_identical_set_without_deleting() {
    let live = vec![res("r1", "m-dead", "", 10), res("r2", "m-live", "", 10)];
    let missions = BTreeSet::from(["m-live".to_string()]);
    let mut deleted = Vec::new();
    let empty = BTreeSet::new();
    let auth = Authoritative {
        live_missions: &missions,
        live_deployments: &empty,
        now_unix: 100,
        dry_run: true,
    };
    let report = reconcile(&live, &auth, &mut |id| {
        deleted.push(id.to_string());
    });
    assert_eq!(report.delete, vec!["r1"], "dry run previews the real set");
    assert!(deleted.is_empty(), "dry run deletes nothing");
    assert!(report.dry_run);
}

#[test]
fn vc_201_059_live_deployment_also_keeps_resource() {
    let live = vec![res("r1", "", "d-live", 10)];
    let deployments = BTreeSet::from(["d-live".to_string()]);
    let empty = BTreeSet::new();
    let auth = Authoritative {
        live_missions: &empty,
        live_deployments: &deployments,
        now_unix: 100,
        dry_run: false,
    };
    let report = reconcile(&live, &auth, &mut |_| {});
    assert_eq!(report.keep, vec!["r1"]);
    assert!(report.delete.is_empty());
}
