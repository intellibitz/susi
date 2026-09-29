use crate::joint_consensus::{overlapping_disjoint_blocked, Electorate, MembershipTransition};

#[test]
fn vc_201_032_joint_quorum_required_from_old_and_new() {
    let mut t = MembershipTransition::new(
        Electorate::from_ids(["a", "b", "c"]),
        Electorate::from_ids(["b", "c", "d"]),
    );
    t.endorse("a");
    t.endorse("b"); // old has 2/3; new has 1/3 — not enough new
    assert!(!t.can_commit());
    t.endorse("d");
    assert!(t.can_commit());
}

#[test]
fn vc_201_032_overlapping_disjoint_rosters_cannot_both_commit() {
    let mut t1 = MembershipTransition::new(
        Electorate::from_ids(["a", "b", "c"]),
        Electorate::from_ids(["a", "b"]),
    );
    let mut t2 = MembershipTransition::new(
        Electorate::from_ids(["a", "b", "c"]),
        Electorate::from_ids(["c", "d"]),
    );
    for m in ["a", "b"] {
        t1.endorse(m);
    }
    for m in ["c", "d"] {
        t2.endorse(m);
    }
    // t2 lacks old quorum (needs 2 of a,b,c)
    assert!(!t2.can_commit());
    assert!(overlapping_disjoint_blocked(&t1, &t2));
    t2.endorse("a");
    t2.endorse("c");
    // Now both could commit with disjoint news — model flags the hazard.
    assert!(t1.can_commit() && t2.can_commit());
    assert!(
        !overlapping_disjoint_blocked(&t1, &t2),
        "serialized joint consensus must refuse concurrent disjoint commits"
    );
}

#[test]
fn vc_201_032_serialized_same_new_roster_ok() {
    let mut t1 = MembershipTransition::new(
        Electorate::from_ids(["a", "b"]),
        Electorate::from_ids(["a", "b", "c"]),
    );
    let mut t2 = MembershipTransition::new(
        Electorate::from_ids(["a", "b"]),
        Electorate::from_ids(["a", "b", "c"]),
    );
    for m in ["a", "b", "c"] {
        t1.endorse(m);
        t2.endorse(m);
    }
    assert!(overlapping_disjoint_blocked(&t1, &t2));
}
