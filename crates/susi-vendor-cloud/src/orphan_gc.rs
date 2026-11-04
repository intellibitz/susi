//! Reconcile orphaned managed cloud resources (VC-201-059).
//!
//! Only resources that carry SUSI ownership tags are candidates for
//! collection — a foreign resource in the account is never touched.
//! Owned resources survive while their mission or deployment is still in
//! authoritative state, while `retain` is set, or until their expiry;
//! everything else is listed for deletion. `DryRun` reports the exact
//! same set it would delete without deleting it.

use std::collections::BTreeSet;

/// A resource visible in the cloud account.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub id: String,
    /// SUSI-ownership tag value; `None` = not ours — hands off.
    pub owner: Option<String>,
    /// Mission tag that created it (empty when untagged).
    pub mission: String,
    /// Deployment tag that created it (empty when untagged).
    pub deployment: String,
    /// Operator-set keep flag — outlives mission end.
    pub retain: bool,
    /// Unix time after which an orphaned resource may be collected.
    pub expires_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Owned, orphaned, past-expiry resources slated for deletion.
    pub delete: Vec<String>,
    /// Owned but still live/retained/unexpired — kept.
    pub keep: Vec<String>,
    /// Untagged (not SUSI-owned) — never considered.
    pub foreign: Vec<String>,
    /// True when nothing was actually deleted.
    pub dry_run: bool,
}

/// Authoritative state the live listing is reconciled against.
#[derive(Debug)]
pub struct Authoritative<'a> {
    /// Mission ids that still exist — a resource tagged to one is kept.
    pub live_missions: &'a BTreeSet<String>,
    /// Deployment ids that still exist.
    pub live_deployments: &'a BTreeSet<String>,
    /// Current unix time for the expiry check.
    pub now_unix: u64,
    /// When true nothing is deleted; the report previews the exact set.
    pub dry_run: bool,
}

/// Reconcile the live cloud listing against authoritative mission and
/// deployment state.
///
/// `delete_fn` runs only when `dry_run` is false — the report lists the
/// same `delete` set either way so a dry run previews the exact change.
#[must_use]
pub fn reconcile(
    live: &[Resource],
    auth: &Authoritative<'_>,
    delete_fn: &mut dyn FnMut(&str),
) -> ReconcileReport {
    let live_missions = auth.live_missions;
    let live_deployments = auth.live_deployments;
    let now_unix = auth.now_unix;
    let dry_run = auth.dry_run;
    let mut report = ReconcileReport {
        delete: Vec::new(),
        keep: Vec::new(),
        foreign: Vec::new(),
        dry_run,
    };
    for r in live {
        if r.owner.is_none() {
            report.foreign.push(r.id.clone());
            continue;
        }
        let mission_live = !r.mission.is_empty() && live_missions.contains(&r.mission);
        let deployment_live = !r.deployment.is_empty() && live_deployments.contains(&r.deployment);
        let orphaned = !mission_live && !deployment_live;
        if r.retain || !orphaned || r.expires_unix > now_unix {
            report.keep.push(r.id.clone());
            continue;
        }
        report.delete.push(r.id.clone());
        if !dry_run {
            delete_fn(&r.id);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_resources() -> Vec<Resource> {
        vec![
            Resource {
                id: "foreign-1".into(),
                owner: None,
                mission: String::new(),
                deployment: String::new(),
                retain: false,
                expires_unix: 100,
            },
            Resource {
                id: "keep-retained".into(),
                owner: Some("susi".into()),
                mission: "m-dead".into(),
                deployment: String::new(),
                retain: true,
                expires_unix: 100,
            },
            Resource {
                id: "keep-live-mission".into(),
                owner: Some("susi".into()),
                mission: "m-live".into(),
                deployment: String::new(),
                retain: false,
                expires_unix: 100,
            },
            Resource {
                id: "keep-live-deployment".into(),
                owner: Some("susi".into()),
                mission: String::new(),
                deployment: "d-live".into(),
                retain: false,
                expires_unix: 100,
            },
            Resource {
                id: "keep-unexpired".into(),
                owner: Some("susi".into()),
                mission: "m-dead".into(),
                deployment: String::new(),
                retain: false,
                expires_unix: 1_500,
            },
            Resource {
                id: "del-expired-1".into(),
                owner: Some("susi".into()),
                mission: "m-dead".into(),
                deployment: String::new(),
                retain: false,
                expires_unix: 1_000,
            },
            Resource {
                id: "del-expired-2".into(),
                owner: Some("susi".into()),
                mission: String::new(),
                deployment: String::new(),
                retain: false,
                expires_unix: 500,
            },
        ]
    }

    #[test]
    fn reconcile_separates_foreign_kept_and_expired_orphaned_resources() {
        let live_missions = BTreeSet::from(["m-live".to_string()]);
        let live_deployments = BTreeSet::from(["d-live".to_string()]);
        let resources = sample_resources();
        let mut deleted = Vec::new();

        let report = reconcile(
            &resources,
            &Authoritative {
                live_missions: &live_missions,
                live_deployments: &live_deployments,
                now_unix: 1_000,
                dry_run: false,
            },
            &mut |id| deleted.push(id.to_string()),
        );

        assert!(!report.dry_run);
        assert_eq!(report.foreign, vec!["foreign-1".to_string()]);
        assert_eq!(
            report.keep,
            vec![
                "keep-retained".to_string(),
                "keep-live-mission".to_string(),
                "keep-live-deployment".to_string(),
                "keep-unexpired".to_string(),
            ]
        );
        assert_eq!(
            report.delete,
            vec!["del-expired-1".to_string(), "del-expired-2".to_string()]
        );
        assert_eq!(deleted, report.delete);
    }

    #[test]
    fn reconcile_dry_run_previews_exact_delete_set_without_invoking_callback() {
        let live_missions = BTreeSet::from(["m-live".to_string()]);
        let live_deployments = BTreeSet::from(["d-live".to_string()]);
        let resources = sample_resources();
        let mut dry_deleted = Vec::new();
        let mut live_deleted = Vec::new();

        let dry_report = reconcile(
            &resources,
            &Authoritative {
                live_missions: &live_missions,
                live_deployments: &live_deployments,
                now_unix: 1_000,
                dry_run: true,
            },
            &mut |id| dry_deleted.push(id.to_string()),
        );
        let live_report = reconcile(
            &resources,
            &Authoritative {
                live_missions: &live_missions,
                live_deployments: &live_deployments,
                now_unix: 1_000,
                dry_run: false,
            },
            &mut |id| live_deleted.push(id.to_string()),
        );

        assert!(dry_report.dry_run);
        assert!(dry_deleted.is_empty());
        assert_eq!(dry_report.delete, live_report.delete);
        assert_eq!(dry_report.keep, live_report.keep);
        assert_eq!(dry_report.foreign, live_report.foreign);
        assert_eq!(
            live_deleted,
            vec!["del-expired-1".to_string(), "del-expired-2".to_string()]
        );
    }

    #[test]
    fn reconcile_enforces_exact_expiry_boundary_and_mixed_mission_deployment_liveness() {
        let live_missions = BTreeSet::from(["m-live".to_string()]);
        let live_deployments = BTreeSet::from(["d-live".to_string()]);
        let resources = vec![
            Resource {
                id: "exact-expiry".into(),
                owner: Some("susi".into()),
                mission: "m-dead".into(),
                deployment: "d-dead".into(),
                retain: false,
                expires_unix: 2_000,
            },
            Resource {
                id: "one-sec-future".into(),
                owner: Some("susi".into()),
                mission: "m-dead".into(),
                deployment: "d-dead".into(),
                retain: false,
                expires_unix: 2_001,
            },
            Resource {
                id: "dead-mission-live-deploy".into(),
                owner: Some("susi".into()),
                mission: "m-dead".into(),
                deployment: "d-live".into(),
                retain: false,
                expires_unix: 100,
            },
        ];
        let mut deleted = Vec::new();
        let report = reconcile(
            &resources,
            &Authoritative {
                live_missions: &live_missions,
                live_deployments: &live_deployments,
                now_unix: 2_000,
                dry_run: false,
            },
            &mut |id| deleted.push(id.to_string()),
        );

        assert_eq!(report.delete, vec!["exact-expiry".to_string()]);
        assert_eq!(deleted, vec!["exact-expiry".to_string()]);
        assert_eq!(
            report.keep,
            vec![
                "one-sec-future".to_string(),
                "dead-mission-live-deploy".to_string(),
            ]
        );
        assert!(report.foreign.is_empty());
    }
}
