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
