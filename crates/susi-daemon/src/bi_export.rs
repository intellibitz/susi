//! BI Data Export (Swarm OS Bullet 80)
//!
//! Renders operational data as CSV — the lowest-common-denominator format
//! every BI tool (Tableau, Looker, Excel, a Python notebook) can ingest
//! without a custom connector.

use crate::cost_analyzer::CostBreakdown;
use crate::leaderboard::LeaderboardEntry;

/// Renders per-key cost breakdowns as CSV: `<dimension_label>,tokens,cost_usd`.
pub fn cost_breakdown_csv(dimension_label: &str, rows: &[(String, CostBreakdown)]) -> String {
    let mut csv = format!("{dimension_label},tokens,cost_usd\n");
    for (key, breakdown) in rows {
        csv.push_str(&format!(
            "{},{},{:.6}\n",
            csv_escape(key),
            breakdown.tokens,
            breakdown.cost_usd
        ));
    }
    csv
}

/// Renders a leaderboard as CSV: `rank,cell_id,trust_score,last_heartbeat`.
pub fn leaderboard_csv(entries: &[LeaderboardEntry]) -> String {
    let mut csv = "rank,cell_id,trust_score,last_heartbeat\n".to_string();
    for entry in entries {
        csv.push_str(&format!(
            "{},{},{},{}\n",
            entry.rank,
            csv_escape(&entry.cell_id),
            entry.trust_score,
            entry.last_heartbeat
        ));
    }
    csv
}

/// Quotes a CSV field per RFC 4180 when it contains a comma, quote, or
/// newline; doubles any embedded quotes.
fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_breakdown_csv_has_header_and_rows() {
        let rows = vec![
            (
                "cell-a".to_string(),
                CostBreakdown {
                    tokens: 100,
                    cost_usd: 0.2,
                },
            ),
            (
                "cell-b".to_string(),
                CostBreakdown {
                    tokens: 50,
                    cost_usd: 0.1,
                },
            ),
        ];
        let csv = cost_breakdown_csv("cell_id", &rows);
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("cell_id,tokens,cost_usd"));
        assert_eq!(lines.next(), Some("cell-a,100,0.200000"));
        assert_eq!(lines.next(), Some("cell-b,50,0.100000"));
    }

    #[test]
    fn leaderboard_csv_has_header_and_rows() {
        let entries = vec![LeaderboardEntry {
            rank: 1,
            cell_id: "cell-a".to_string(),
            trust_score: 0.9,
            last_heartbeat: 42,
        }];
        let csv = leaderboard_csv(&entries);
        assert_eq!(
            csv,
            "rank,cell_id,trust_score,last_heartbeat\n1,cell-a,0.9,42\n"
        );
    }

    #[test]
    fn fields_with_commas_are_quoted() {
        let rows = vec![(
            "model, v2".to_string(),
            CostBreakdown {
                tokens: 1,
                cost_usd: 0.0,
            },
        )];
        let csv = cost_breakdown_csv("model", &rows);
        assert!(csv.contains("\"model, v2\""));
    }
}
