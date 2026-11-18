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

#[test]
fn joint_consensus_try_commit_without_pending_proposal_returns_false() {
    let mut dag = MissionDag::new("no pending proposal");
    dag.bootstrap_roster(["a", "b", "c"]);
    assert!(dag.pending_membership.is_none());
    assert!(!dag.try_commit_membership(None));
    assert_eq!(dag.roster, Electorate::from_ids(["a", "b", "c"]));
}

#[test]
fn joint_consensus_ignores_outsider_endorsements() {
    let mut dag = MissionDag::new("outsider endorsements");
    dag.bootstrap_roster(["a", "b", "c"]);
    dag.propose_membership(Electorate::from_ids(["b", "c", "d"]));

    dag.endorse_membership("outsider-1");
    dag.endorse_membership("outsider-2");
    dag.endorse_membership("b");
    assert!(!dag.try_commit_membership(None));
    assert_eq!(dag.roster, Electorate::from_ids(["a", "b", "c"]));
}

#[test]
fn joint_consensus_supports_sequential_quorum_transitions() {
    let mut dag = MissionDag::new("sequential transitions");
    dag.bootstrap_roster(["a", "b", "c"]);

    dag.propose_membership(Electorate::from_ids(["b", "c", "d"]));
    for m in ["b", "c"] {
        dag.endorse_membership(m);
    }
    assert!(dag.try_commit_membership(None));
    assert_eq!(dag.roster, Electorate::from_ids(["b", "c", "d"]));

    dag.propose_membership(Electorate::from_ids(["c", "d", "e"]));
    for m in ["c", "d"] {
        dag.endorse_membership(m);
    }
    assert!(dag.try_commit_membership(None));
    assert_eq!(dag.roster, Electorate::from_ids(["c", "d", "e"]));
}

#[test]
fn joint_consensus_electorate_quorum_size_scales_with_majority_floor() {
    assert_eq!(Electorate::from_ids(["n1"]).quorum_size(), 1);
    assert_eq!(Electorate::from_ids(["n1", "n2"]).quorum_size(), 2);
    assert_eq!(Electorate::from_ids(["n1", "n2", "n3"]).quorum_size(), 2);
    assert_eq!(
        Electorate::from_ids(["n1", "n2", "n3", "n4"]).quorum_size(),
        3
    );
    assert_eq!(
        Electorate::from_ids(["n1", "n2", "n3", "n4", "n5"]).quorum_size(),
        3
    );
}
