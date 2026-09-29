//! Chaos tests for leader election and partitions (VC-201-095).

use crate::swarm_chaos::ChaosNetwork;

#[test]
fn swarm_chaos_election_is_deterministic() {
    let mut net = ChaosNetwork::default();
    net.add("a");
    net.add("b");
    net.add("c");
    let candidates = vec![
        ("a".into(), 0.5f32),
        ("b".into(), 0.9f32),
        ("c".into(), 0.9f32),
    ];
    for n in net.nodes.values_mut() {
        n.elect(&candidates);
    }
    assert!(net.nodes.values().all(|n| n.leader == "c")); // tie → lex max? wait trust equal → lex: b < c so c
    net.assert_no_split_brain_commit().unwrap();
}

#[test]
fn swarm_chaos_partition_delays_then_no_split_brain() {
    let mut net = ChaosNetwork::default();
    net.add("a");
    net.add("b");
    for n in net.nodes.values_mut() {
        n.elect(&[("a".into(), 1.0)]);
    }
    let rec = net.nodes["a"]
        .propose_commit(1, "hello")
        .expect("leader proposes");
    net.partition("b", true);
    net.broadcast("a", rec.clone(), false);
    // b did not accept yet
    assert!(net.nodes["b"].log.is_empty());
    net.partition("b", false);
    net.flush_delayed();
    assert_eq!(net.nodes["b"].log.len(), 1);
    net.assert_no_split_brain_commit().unwrap();
}

#[test]
fn swarm_chaos_duplicated_messages_are_idempotent() {
    let mut net = ChaosNetwork::default();
    net.add("a");
    net.add("b");
    for n in net.nodes.values_mut() {
        n.elect(&[("a".into(), 1.0)]);
    }
    let rec = net.nodes["a"].propose_commit(7, "dup").unwrap();
    net.broadcast("a", rec, true);
    assert_eq!(net.nodes["b"].log.len(), 1);
    net.assert_no_split_brain_commit().unwrap();
}

#[test]
fn swarm_chaos_rejects_same_term_foreign_leader_commit() {
    let mut net = ChaosNetwork::default();
    net.add("a");
    net.add("b");
    net.nodes.get_mut("a").unwrap().elect(&[("a".into(), 1.0)]);
    net.nodes.get_mut("b").unwrap().term = 1;
    net.nodes.get_mut("b").unwrap().leader = "b".into();
    let rec = net.nodes["a"].propose_commit(1, "x").unwrap();
    assert!(!net.nodes.get_mut("b").unwrap().accept(rec.clone()));
    // a accepts its own commit; b rejects — no split-brain *commit* on the log.
    assert!(net.nodes.get_mut("a").unwrap().accept(rec));
    assert!(net.nodes["b"].log.is_empty());
    assert_eq!(net.nodes["a"].log.len(), 1);
}
