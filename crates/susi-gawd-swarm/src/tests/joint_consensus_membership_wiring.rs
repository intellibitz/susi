//! Wiring: joint-consensus quorum for swarm roster changes (T-INTELLIBITZ-13).

use crate::dag::MissionDag;
use crate::joint_consensus::{Electorate, MembershipTransition};

#[test]
fn joint_consensus_membership_wiring() {
    let mut dag = MissionDag::new("roster quorum wiring");
    dag.bootstrap_roster(["a", "b", "c"]);
    assert_eq!(dag.roster.0.len(), 3);

    // Propose add-d; insufficient endorsements must not commit.
    dag.propose_membership(Electorate::from_ids(["b", "c", "d"]));
    dag.endorse_membership("a");
    dag.endorse_membership("b"); // old quorum ok; new lacks d
    assert!(!dag.try_commit_membership(None));
    assert_eq!(dag.roster, Electorate::from_ids(["a", "b", "c"]));
    assert!(dag.pending_membership.is_some());

    // Complete joint quorum → roster advances.
    dag.endorse_membership("d");
    assert!(dag.try_commit_membership(None));
    assert_eq!(dag.roster, Electorate::from_ids(["b", "c", "d"]));
    assert!(dag.pending_membership.is_none());

    // Overlapping transition that would leave a disjoint roster is refused.
    dag.propose_membership(Electorate::from_ids(["b", "c"]));
    for m in ["b", "c"] {
        dag.endorse_membership(m);
    }
    let mut competing = MembershipTransition::new(
        Electorate::from_ids(["b", "c", "d"]),
        Electorate::from_ids(["d", "e"]),
    );
    for m in ["b", "d", "e"] {
        competing.endorse(m);
    }
    assert!(competing.can_commit());
    assert!(dag
        .pending_membership
        .as_ref()
        .is_some_and(|t| t.can_commit()));
    assert!(
        !dag.try_commit_membership(Some(&competing)),
        "disjoint concurrent commits must be blocked"
    );
    assert_eq!(
        dag.roster,
        Electorate::from_ids(["b", "c", "d"]),
        "roster unchanged when hazard detected"
    );
}
