//! A/B swarms (Swarm OS Bullet 27)
//!
//! Two strategies run as separate arms. The winner is the arm with the
//! strictly higher success rate. A tie, or two arms with no trials,
//! selects nobody.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    pub name: String,
    pub successes: u64,
    pub trials: u64,
}

/// `Some(name)` of the strictly better arm. Comparison is cross-multiplied
/// so it does not depend on floating point.
pub fn select_winner<'a>(left: &'a Arm, right: &'a Arm) -> Option<&'a str> {
    if left.trials == 0 && right.trials == 0 {
        return None;
    }
    let left_score = u128::from(left.successes) * u128::from(right.trials);
    let right_score = u128::from(right.successes) * u128::from(left.trials);
    match left_score.cmp(&right_score) {
        std::cmp::Ordering::Greater => Some(left.name.as_str()),
        std::cmp::Ordering::Less => Some(right.name.as_str()),
        std::cmp::Ordering::Equal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_higher_success_rate_wins_and_a_tie_selects_nobody() {
        let better = Arm {
            name: "a".into(),
            successes: 3,
            trials: 4,
        };
        let worse = Arm {
            name: "b".into(),
            successes: 1,
            trials: 4,
        };
        assert_eq!(select_winner(&better, &worse), Some("a"));
        assert_eq!(select_winner(&better, &better), None);
    }
}
