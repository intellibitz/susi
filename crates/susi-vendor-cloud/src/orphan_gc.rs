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
