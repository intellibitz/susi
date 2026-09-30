//! A/B experiments with statistical tests on routing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbResult {
    pub winner: Option<String>,
    pub significant: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmStats<'a> {
    pub name: &'a str,
    pub ok: u32,
    pub n: u32,
}

/// Simple two-proportion gate (threshold on rate delta).
#[must_use]
pub fn ab_decide(a: ArmStats<'_>, b: ArmStats<'_>) -> AbResult {
    if a.n == 0 || b.n == 0 {
        return AbResult {
            winner: None,
            significant: false,
        };
    }
    let ra = f64::from(a.ok) / f64::from(a.n);
    let rb = f64::from(b.ok) / f64::from(b.n);
    let delta = (ra - rb).abs();
    let significant = delta >= 0.1 && a.n.min(b.n) >= 20;
    let winner = if !significant {
        None
    } else if ra > rb {
        Some(a.name.into())
    } else {
        Some(b.name.into())
    };
    AbResult {
        winner,
        significant,
    }
}

#[cfg(test)]
mod brain_experiments_tests {
    use super::*;

    #[test]
    fn brain_experiments_needs_enough_samples() {
        assert!(
            !ab_decide(
                ArmStats {
                    name: "a",
                    ok: 9,
                    n: 10
                },
                ArmStats {
                    name: "b",
                    ok: 1,
                    n: 10
                }
            )
            .significant
        );
        let r = ab_decide(
            ArmStats {
                name: "a",
                ok: 18,
                n: 20,
            },
            ArmStats {
                name: "b",
                ok: 4,
                n: 20,
            },
        );
        assert!(r.significant);
        assert_eq!(r.winner.as_deref(), Some("a"));
    }
}
