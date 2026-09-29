//! Model-check membership and term transitions (VC-201-031).

use crate::membership_model::{ClusterModel, Message, TermState, SAFETY_PROPERTIES};

#[test]
fn vc_201_031_safety_properties_documented() {
    assert!(SAFETY_PROPERTIES.len() >= 4);
    assert!(SAFETY_PROPERTIES.iter().any(|p| p.contains("split")));
}

#[test]
fn vc_201_031_reordered_claims_same_leader_no_term_inflation() {
    let mut c = ClusterModel::default();
    c.add("n1");
    c.add("n2");
    c.deliver(
        "n1",
        &Message::Claim {
            leader: "n1".into(),
        },
    );
    let t1 = c.nodes["n1"].durable.term;
    // Same leader re-claim must not inflate term.
    c.deliver(
        "n1",
        &Message::Claim {
            leader: "n1".into(),
        },
    );
    assert_eq!(c.nodes["n1"].durable.term, t1);
    c.no_split_brain_connected().expect("no split");
}

#[test]
fn vc_201_031_partition_allows_divergent_terms_reconnect_adopts_higher() {
    let mut c = ClusterModel::default();
    c.add("a");
    c.add("b");
    c.deliver("a", &Message::Claim { leader: "a".into() });
    c.set_partition("b", true);
    c.deliver("b", &Message::Claim { leader: "b".into() }); // dropped
                                                            // b still at term 0; a at term 1.
    assert_eq!(c.nodes["a"].durable.term, 1);
    assert_eq!(c.nodes["b"].durable.term, 0);
    c.set_partition("b", false);
    let term_a = c.nodes["a"].durable.term;
    let leader_a = c.nodes["a"].durable.leader.clone();
    c.deliver(
        "b",
        &Message::Observe {
            term: term_a,
            leader: leader_a,
        },
    );
    assert_eq!(c.nodes["b"].durable.term, term_a);
    assert_eq!(c.nodes["b"].durable.leader, "a");
    c.no_split_brain_connected().expect("healed");
}

#[test]
fn vc_201_031_exhaustive_message_schedules_preserve_sp3() {
    let leaders = ["a", "b"];
    let mut counterexamples = Vec::new();
    // Schedules: for each of 4 slots, deliver Claim(a)|Claim(b)|Observe to both nodes.
    for mask in 0..64u32 {
        let mut c = ClusterModel::default();
        c.add("a");
        c.add("b");
        for slot in 0..3u32 {
            let choice = (mask >> (slot * 2)) & 0b11;
            let msg = match choice {
                0 => Message::Claim { leader: "a".into() },
                1 => Message::Claim { leader: "b".into() },
                2 => {
                    let t = c.nodes["a"].durable.term.max(c.nodes["b"].durable.term);
                    let leader = if t == c.nodes["a"].durable.term {
                        c.nodes["a"].durable.leader.clone()
                    } else {
                        c.nodes["b"].durable.leader.clone()
                    };
                    Message::Observe { term: t, leader }
                }
                _ => Message::Restart,
            };
            let before_a = c.nodes["a"].durable.clone();
            let before_b = c.nodes["b"].durable.clone();
            c.deliver("a", &msg);
            c.deliver("b", &msg);
            assert!(ClusterModel::terms_monotonic_after(
                &before_a,
                &c.nodes["a"].durable
            ));
            assert!(ClusterModel::terms_monotonic_after(
                &before_b,
                &c.nodes["b"].durable
            ));
            if let Err(e) = c.no_split_brain_connected() {
                counterexamples.push((mask, slot, e));
            }
        }
        let _ = leaders;
        let _ = TermState::default();
    }
    assert!(
        counterexamples.is_empty(),
        "SP3 counterexamples: {counterexamples:?}"
    );
}

#[test]
fn vc_201_031_leadership_change_bumps_term() {
    let mut c = ClusterModel::default();
    c.add("n");
    c.deliver("n", &Message::Claim { leader: "n".into() });
    assert_eq!(c.nodes["n"].durable.term, 1);
    c.deliver(
        "n",
        &Message::Claim {
            leader: "other".into(),
        },
    );
    assert_eq!(c.nodes["n"].durable.term, 2);
    assert_eq!(c.nodes["n"].durable.leader, "other");
}
