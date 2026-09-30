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

/// Wiring test: swarm roster changes commit only under joint-consensus quorum.
///
/// Both old and new electorates must endorse with majority quorum;
/// overlapping disjoint new rosters sharing an old member are blocked.
#[test]
fn joint_consensus_membership_wiring() {
    // t1: old [a,b,c] → new [a,b]; t2: old [a,b,c] → new [c,d].
    // They share old member 'a' but their new rosters are disjoint.
    let mut t1 = MembershipTransition::new(
        Electorate::from_ids(["a", "b", "c"]),
        Electorate::from_ids(["a", "b"]),
    );
    let mut t2 = MembershipTransition::new(
        Electorate::from_ids(["a", "b", "c"]),
        Electorate::from_ids(["c", "d"]),
    );
    // Neither has full quorum yet — both blocked → hazard not materialized.
    assert!(!t1.can_commit());
    assert!(!t2.can_commit());
    assert!(overlapping_disjoint_blocked(&t1, &t2));

    // Give t2 full joint quorum (old: a,c; new: c,d).
    t2.endorse("a");
    t2.endorse("c");
    t2.endorse("d");
    assert!(t2.can_commit());
    // t1 still blocked → hazard still blocked.
    assert!(overlapping_disjoint_blocked(&t1, &t2));

    // Give t1 full joint quorum too (old: a,b,c; new: a,b).
    t1.endorse("a");
    t1.endorse("b");
    t1.endorse("c");
    assert!(t1.can_commit() && t2.can_commit());
    // Both committed with disjoint new rosters sharing old 'a' → hazard flagged.
    assert!(!overlapping_disjoint_blocked(&t1, &t2));

    // Same new roster (non-disjoint) is allowed under the fault model.
    let mut s1 = MembershipTransition::new(
        Electorate::from_ids(["a", "b"]),
        Electorate::from_ids(["a", "b", "c"]),
    );
    let mut s2 = MembershipTransition::new(
        Electorate::from_ids(["a", "b"]),
        Electorate::from_ids(["a", "b", "c"]),
    );
    for m in ["a", "b", "c"] {
        s1.endorse(m);
        s2.endorse(m);
    }
    assert!(s1.can_commit() && s2.can_commit());
    assert!(overlapping_disjoint_blocked(&s1, &s2));
}
