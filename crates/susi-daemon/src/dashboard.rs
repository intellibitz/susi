//! Swarm Health Dashboard (Swarm OS Bullet 72)
//!
//! Renders one report from the swarm-level metrics snapshot (Bullet 30),
//! the SLA check against it (Bullet 75), and the trust leaderboard
//! (Bullet 79). Rendering does not deposit pheromones — alerting stays
//! on `SwarmBlackboard::check_sla_and_alert`.

use crate::leaderboard::LeaderboardEntry;
use crate::sla_monitor::SlaViolation;
use crate::swarm_metrics::SwarmMetricsSnapshot;

/// Plain-text swarm health report: throughput, SLA status, and the
/// current trust ranking.
pub fn render_health_dashboard(
    snapshot: &SwarmMetricsSnapshot,
    violations: &[SlaViolation],
    leaderboard: &[LeaderboardEntry],
) -> String {
    let mut out = String::new();
    out.push_str("# Swarm health\n");
    out.push_str(&format!(
        "missions_started: {}\n",
        snapshot.missions_started
    ));
    out.push_str(&format!(
        "missions_completed: {}\n",
        snapshot.missions_completed
    ));
    out.push_str(&format!(
        "missions_succeeded: {}\n",
        snapshot.missions_succeeded
    ));
    out.push_str(&success_rate_line(snapshot));
    out.push('\n');
    out.push_str(&format!(
        "avg_resolution_ms: {}\n",
        snapshot.avg_time_to_resolution_ms
    ));
    out.push_str(&format!(
        "distinct_solutions: {}\n",
        snapshot.distinct_solutions
    ));

    out.push_str("\n## SLA\n");
    if violations.is_empty() {
        out.push_str("ok\n");
    } else {
        for violation in violations {
            out.push_str(&violation_line(violation));
            out.push('\n');
        }
    }

    out.push_str("\n## Leaderboard\n");
    if leaderboard.is_empty() {
        out.push_str("no cells registered\n");
    } else {
        for entry in leaderboard {
            out.push_str(&format!(
                "{}. {} (trust {:.2})\n",
                entry.rank, entry.cell_id, entry.trust_score
            ));
        }
    }
    out
}

fn success_rate_line(snapshot: &SwarmMetricsSnapshot) -> String {
    let Some(pct) = snapshot
        .missions_succeeded
        .checked_mul(100)
        .and_then(|scaled| scaled.checked_div(snapshot.missions_completed))
    else {
        return "success_rate: n/a".to_string();
    };
    format!(
        "success_rate: {pct}% ({}/{})",
        snapshot.missions_succeeded, snapshot.missions_completed
    )
}

fn violation_line(violation: &SlaViolation) -> String {
    match violation {
        SlaViolation::LatencyExceeded {
            target_ms,
            actual_ms,
        } => format!("- latency {actual_ms}ms exceeds target {target_ms}ms"),
        SlaViolation::SuccessRateBelowTarget { target, actual } => {
            format!("- success rate {actual:.3} below target {target:.3}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_failed() -> SwarmMetricsSnapshot {
        SwarmMetricsSnapshot {
            missions_started: 2,
            missions_completed: 2,
            missions_succeeded: 1,
            distinct_solutions: 2,
            avg_time_to_resolution_ms: 4000,
        }
    }

    #[test]
    fn report_includes_metrics_sla_miss_and_ranked_cells() {
        let violations = vec![SlaViolation::LatencyExceeded {
            target_ms: 1000,
            actual_ms: 4000,
        }];
        let board = vec![LeaderboardEntry {
            rank: 1,
            cell_id: "cell-a".to_string(),
            trust_score: 0.9,
            last_heartbeat: 1,
        }];
        let report = render_health_dashboard(&snapshot_failed(), &violations, &board);
        assert!(report.contains("missions_completed: 2"));
        assert!(report.contains("success_rate: 50% (1/2)"));
        assert!(report.contains("latency 4000ms exceeds target 1000ms"));
        assert!(report.contains("1. cell-a"));
    }

    #[test]
    fn empty_inputs_render_na_ok_and_no_cells() {
        let report = render_health_dashboard(&SwarmMetricsSnapshot::default(), &[], &[]);
        assert!(report.contains("success_rate: n/a"));
        assert!(report.contains("## SLA\nok\n"));
        assert!(report.contains("no cells registered"));
    }
}
