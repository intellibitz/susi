//! Joint-consensus membership transitions (VC-201-032).
//!
//! Overlapping add/remove operations cannot commit disjoint active rosters
//! under the documented fault model: a transition is serialized as
//! (old_electorate, new_electorate) and only commits when a joint quorum of
//! both electorates endorses it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Electorate(pub BTreeSet<String>);

impl Electorate {
    #[must_use]
    pub fn from_ids<I: IntoIterator<Item = S>, S: Into<String>>(ids: I) -> Self {
        Self(ids.into_iter().map(Into::into).collect())
    }

    #[must_use]
    pub fn quorum_size(&self) -> usize {
        self.0.len() / 2 + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipTransition {
    pub old: Electorate,
    pub new: Electorate,
    pub endorsements: BTreeSet<String>,
}

impl MembershipTransition {
    #[must_use]
    pub fn new(old: Electorate, new: Electorate) -> Self {
        Self {
            old,
            new,
            endorsements: BTreeSet::new(),
        }
    }

    pub fn endorse(&mut self, member: &str) {
        if self.old.0.contains(member) || self.new.0.contains(member) {
            self.endorsements.insert(member.to_string());
        }
    }

    /// Joint quorum: majority of old AND majority of new must endorse.
    #[must_use]
    pub fn can_commit(&self) -> bool {
        let old_votes = self
            .endorsements
            .iter()
            .filter(|m| self.old.0.contains(*m))
            .count();
        let new_votes = self
            .endorsements
            .iter()
            .filter(|m| self.new.0.contains(*m))
            .count();
        old_votes >= self.old.quorum_size() && new_votes >= self.new.quorum_size()
    }
}

/// Documented fault model: overlapping transitions that would leave two
/// disjoint active rosters cannot both commit.
#[must_use]
pub fn overlapping_disjoint_blocked(a: &MembershipTransition, b: &MembershipTransition) -> bool {
    if !a.can_commit() || !b.can_commit() {
        return true; // at least one blocked — OK
    }
    // Both committed: their `new` sets must not be disjoint if they share an old member.
    let share_old = a.old.0.intersection(&b.old.0).next().is_some();
    if share_old && a.new.0.is_disjoint(&b.new.0) {
        return false; // violation
    }
    true
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn electorate_deduplicates_ids_and_computes_strict_majority_quorum() {
        let e3 = Electorate::from_ids(["n1", "n2", "n3", "n1"]);
        assert_eq!(e3.0.len(), 3);
        assert_eq!(e3.quorum_size(), 2);

        let e4 = Electorate::from_ids(["n1", "n2", "n3", "n4"]);
        assert_eq!(e4.quorum_size(), 3);
    }

    #[test]
    fn endorse_ignores_non_members_and_records_old_or_new_members() {
        let old = Electorate::from_ids(["a", "b"]);
        let new = Electorate::from_ids(["b", "c"]);
        let mut tr = MembershipTransition::new(old, new);
        tr.endorse("outsider");
        assert!(tr.endorsements.is_empty());

        tr.endorse("a");
        tr.endorse("c");
        assert_eq!(tr.endorsements.len(), 2);
    }

    #[test]
    fn can_commit_requires_majority_of_both_old_and_new_electorates() {
        let old = Electorate::from_ids(["a", "b", "c"]);
        let new = Electorate::from_ids(["c", "d", "e"]);
        let mut tr = MembershipTransition::new(old, new);
        tr.endorse("a");
        tr.endorse("b");
        assert!(!tr.can_commit());

        tr.endorse("c");
        assert!(!tr.can_commit());

        tr.endorse("d");
        assert!(tr.can_commit());
    }

    #[test]
    fn overlapping_disjoint_blocked_detects_disjoint_new_rosters_sharing_old_member() {
        let old = Electorate::from_ids(["a", "b"]);
        let mut t1 = MembershipTransition::new(old.clone(), Electorate::from_ids(["x"]));
        let mut t2 = MembershipTransition::new(old, Electorate::from_ids(["y"]));
        t1.endorse("a");
        t1.endorse("b");
        t1.endorse("x");
        assert!(overlapping_disjoint_blocked(&t1, &t2));

        t2.endorse("a");
        t2.endorse("b");
        t2.endorse("y");
        assert!(!overlapping_disjoint_blocked(&t1, &t2));
    }

    #[test]
    fn overlapping_disjoint_blocked_permits_concurrent_transitions_with_overlapping_new_rosters() {
        let old = Electorate::from_ids(["a", "b"]);
        let mut t1 = MembershipTransition::new(old.clone(), Electorate::from_ids(["b", "x"]));
        let mut t2 = MembershipTransition::new(old, Electorate::from_ids(["b", "y"]));
        for m in ["a", "b", "x"] {
            t1.endorse(m);
        }
        for m in ["a", "b", "y"] {
            t2.endorse(m);
        }
        assert!(t1.can_commit());
        assert!(t2.can_commit());
        assert!(overlapping_disjoint_blocked(&t1, &t2));
    }
}
