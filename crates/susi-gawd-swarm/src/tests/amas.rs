use super::*;

#[test]
fn test_aspiration_23_universal_swarm_operation() {
    use susi_gawd_agents::agents::{
        GawdAgent, HighDensityContextStore, SafetyAgent, SecurityAgent,
    };
    let tmp_dir =
        std::env::temp_dir().join(format!("susi_swarm_test_asp23-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let blackboard: MissionBlackboard = Arc::new(HighDensityContextStore::new(10));

    let safety = SafetyAgent;
    let security = SecurityAgent;

    let res1 = safety.execute("admin mission: test aspiration 23", &tmp_dir, &blackboard);
    let res2 = security.execute("admin mission: test aspiration 23", &tmp_dir, &blackboard);

    assert!(res1.is_ok());
    assert!(res2.is_ok());

    assert!(blackboard.contains_key("SafetyAgent"));
    assert!(blackboard.contains_key("SecurityAgent"));
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

fn electorate(names: &[&str]) -> std::collections::BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_quorum_majority_commits_on_agreement() {
    let outputs = vec![
        ("AgentA".to_string(), "  The answer is 42.  ".to_string()),
        ("PeerNode_1".to_string(), "the answer is 42.".to_string()),
        (
            "AgentB".to_string(),
            "something totally different".to_string(),
        ),
    ];
    let voters = electorate(&["AgentA", "PeerNode_1", "AgentB"]);
    let quorum = SusiSupervisor::quorum_majority(&voters, &outputs);
    assert_eq!(quorum, Some(("The answer is 42.".to_string(), 2)));
}

#[test]
fn test_quorum_majority_none_without_majority() {
    let outputs = vec![
        ("AgentA".to_string(), "answer one".to_string()),
        ("AgentB".to_string(), "answer two".to_string()),
        ("AgentC".to_string(), "answer three".to_string()),
    ];
    let voters = electorate(&["AgentA", "AgentB", "AgentC"]);
    assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
}

#[test]
fn test_quorum_majority_requires_at_least_two_outputs() {
    let outputs = vec![("AgentA".to_string(), "solo answer".to_string())];
    let voters = electorate(&["AgentA"]);
    assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
}

#[test]
fn test_quorum_majority_thresholds_against_pinned_electorate_not_respondents() {
    // 5 voters were dispatched; 2 responded and agree. Under the old
    // respondent-threshold rule that claimed a "quorum" of 40% of the
    // voter set — the mid-vote-churn hole. Pinned electorate: 2 < 3.
    let outputs = vec![
        ("AgentA".to_string(), "agreed".to_string()),
        ("PeerNode_1".to_string(), "agreed".to_string()),
    ];
    let voters = electorate(&["AgentA", "AgentB", "AgentC", "PeerNode_1", "PeerNode_2"]);
    assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);

    // 3 of the same 5 agree → real quorum.
    let mut three = outputs.clone();
    three.push(("AgentB".to_string(), "agreed".to_string()));
    assert_eq!(
        SusiSupervisor::quorum_majority(&voters, &three),
        Some(("agreed".to_string(), 3))
    );
}

#[test]
fn test_quorum_majority_ignores_votes_outside_the_pinned_electorate() {
    // A peer verified AFTER the broadcast was never dispatched; its
    // blackboard entry (or any stray key) must not tilt the count.
    let outputs = vec![
        ("AgentA".to_string(), "late-joiner answer".to_string()),
        (
            "PeerNode_late".to_string(),
            "late-joiner answer".to_string(),
        ),
        ("AgentB".to_string(), "other".to_string()),
    ];
    let voters = electorate(&["AgentA", "AgentB"]);
    assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
}

#[test]
fn swarm_gap_durable_quorum_canonicalizes_addresses_and_rejects_errors() {
    let voters = electorate(&["PeerNode_a", "PeerNode_b", "AgentA"]);
    let outputs = vec![
        (
            "PeerNode_a".to_string(),
            "[A2A Flux (10.0.0.1:9093)]: shared answer".to_string(),
        ),
        (
            "PeerNode_b".to_string(),
            "[A2A Flux (10.0.0.2:9093)]: SHARED   ANSWER".to_string(),
        ),
        (
            "AgentA".to_string(),
            "[A2A Error (10.0.0.3:9093)]: stale authority".to_string(),
        ),
        (
            "PeerNode_late".to_string(),
            "[A2A Flux (10.0.0.4:9093)]: shared answer".to_string(),
        ),
    ];
    assert_eq!(
        SusiSupervisor::quorum_majority(&voters, &outputs),
        Some(("shared answer".to_string(), 2))
    );
}

#[test]
fn swarm_gap_durable_quorum_deduplicates_voters_across_member_views() {
    let voters = electorate(&["PeerNode_a", "PeerNode_b", "PeerNode_c"]);
    let outputs = vec![
        (
            "PeerNode_a".to_string(),
            "[A2A Flux (10.0.0.1:9093)]: same".to_string(),
        ),
        (
            "PeerNode_a".to_string(),
            "[A2A Flux (10.0.0.9:9093)]: same".to_string(),
        ),
        (
            "PeerNode_foreign_view".to_string(),
            "[A2A Flux (10.0.0.8:9093)]: same".to_string(),
        ),
        (
            "PeerNode_b".to_string(),
            "[A2A Flux (10.0.0.2:9093)]: different".to_string(),
        ),
    ];
    assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
}

#[test]
fn swarm_gap_durable_quorum_bounded_collection_discards_delayed_valid_peer() {
    let blackboard: MissionBlackboard =
        Arc::new(susi_gawd_agents::agents::HighDensityContextStore::new(8));
    let electorate = electorate(&["PeerNode_delayed"]);
    let (sender, receiver) = std::sync::mpsc::channel();
    let delayed = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        let _ = sender.send(PeerResponse {
            voter: "PeerNode_delayed".to_string(),
            output: "[A2A Flux (10.0.0.7:9093)]: valid but late".to_string(),
        });
    });
    let deadline = std::time::Instant::now() + Duration::from_millis(5);
    let accepted = SusiSupervisor::collect_peer_responses(
        &receiver,
        &electorate,
        &blackboard,
        deadline,
        &CancellationHandle::new(),
    );
    assert_eq!(accepted, 0);
    assert!(!blackboard.contains_key("PeerNode_delayed"));
    delayed.join().expect("delayed peer fixture should exit");
    assert!(!blackboard.contains_key("PeerNode_delayed"));
}

#[test]
fn swarm_gap_durable_quorum_stale_authority_is_not_dispatched() {
    let mut stale = peer_node("stale", 1.0, true, PeerAdmission::Explicit);
    stale.last_seen_secs = now_secs().saturating_sub(PEER_STALE_SECS + 1);
    let fresh = peer_node("fresh", 0.1, true, PeerAdmission::Explicit);
    let discovered = peer_node("discovered", 1.0, true, PeerAdmission::Discovered);
    let nodes = [stale, fresh, discovered];
    let eligible = SusiSupervisor::eligible_quorum_peers(&nodes);
    assert_eq!(eligible.len(), 1);
    assert_eq!(eligible[0].node_id, "fresh");
}

#[test]
fn test_capability_bloom_no_false_negatives() {
    let bloom = CapabilityBloom::from_tokens(["bloat_audit", "sovereign_dashboard", "status"]);
    assert!(bloom.contains("bloat_audit"));
    assert!(bloom.contains("sovereign_dashboard"));
    assert!(bloom.contains("status"));
    // Not inserted: may or may not be a false positive, but must not crash and
    // must behave deterministically for the same input.
    let first = bloom.contains("totally_unrelated_xyz");
    let second = bloom.contains("totally_unrelated_xyz");
    assert_eq!(first, second);
}

fn peer_node(id: &str, trust: f32, active: bool, admission: PeerAdmission) -> ClusterPeerNode {
    ClusterPeerNode {
        node_id: id.into(),
        address: format!("10.0.0.1:9{id:0>3}"),
        node_type: "PEER".into(),
        is_active: active,
        capabilities: vec!["CORE".into()],
        registry_checksum: 0,
        latency_ms: 0,
        uptime_secs: 0,
        trust_score: trust,
        capability_bloom: CapabilityBloom::default(),
        admission,
        last_seen_secs: now_secs(),
        pubkey: String::new(),
        key_bound_at: 0,
        bind_sig: String::new(),
    }
}

#[test]
fn test_elect_leader_picks_highest_trust_verified_node() {
    let roster = vec![
        peer_node("n1", 0.9, true, PeerAdmission::Explicit),
        peer_node("n2", 0.6, true, PeerAdmission::Explicit),
        peer_node("susi-local-master", 1.0, true, PeerAdmission::Local),
    ];
    assert_eq!(
        SusiSupervisor::elect_leader(&roster),
        Some("susi-local-master".to_string())
    );
}

#[test]
fn test_elect_leader_deterministic_tie_break_and_exclusions() {
    // Equal trust: lexicographically greatest node_id wins — every
    // member computing over the same roster must converge.
    let roster = vec![
        peer_node("peer-b", 0.8, true, PeerAdmission::Explicit),
        peer_node("peer-a", 0.8, true, PeerAdmission::Explicit),
    ];
    assert_eq!(
        SusiSupervisor::elect_leader(&roster),
        Some("peer-b".to_string())
    );
    // Discovered and inactive nodes can never lead, however trusted.
    let untrusted = vec![
        peer_node("disc", 1.0, true, PeerAdmission::Discovered),
        peer_node("dead", 1.0, false, PeerAdmission::Explicit),
        peer_node("ok", 0.1, true, PeerAdmission::Explicit),
    ];
    assert_eq!(
        SusiSupervisor::elect_leader(&untrusted),
        Some("ok".to_string())
    );
    assert_eq!(SusiSupervisor::elect_leader(&[]), None);
}

#[test]
fn test_elect_leader_skips_stale_peers() {
    // A peer whose last pong predates PEER_STALE_SECS cannot lead even
    // if its persisted is_active flag was never swept.
    let mut stale = peer_node("stale", 1.0, true, PeerAdmission::Explicit);
    stale.last_seen_secs = now_secs().saturating_sub(PEER_STALE_SECS + 1);
    let fresh = peer_node("fresh", 0.1, true, PeerAdmission::Explicit);
    assert_eq!(
        SusiSupervisor::elect_leader(&[stale.clone(), fresh]),
        Some("fresh".to_string())
    );
    // A roster of only stale non-local nodes elects nobody.
    assert_eq!(SusiSupervisor::elect_leader(&[stale]), None);
    // The local node is never stale — loopback is always live.
    let mut local = peer_node("local", 0.0, true, PeerAdmission::Local);
    local.last_seen_secs = 0;
    assert!(!local.is_stale(now_secs()));
    assert_eq!(
        SusiSupervisor::elect_leader(&[local]),
        Some("local".to_string())
    );
}

#[test]
fn test_capability_bloom_hex_roundtrip() {
    let bloom = CapabilityBloom::from_tokens(["deep_scan", "mcp_scout"]);
    let hex = bloom.to_hex();
    let restored = CapabilityBloom::from_hex(&hex);
    assert_eq!(bloom, restored);
    assert!(restored.contains("deep_scan"));
}

#[test]
fn test_rank_peers_prefers_capability_match_over_raw_trust() {
    let goal_tokens = tokenize_goal("admin pulse: run bloat_audit across the substrate");

    let generic_high_trust = ClusterPeerNode {
        node_id: "n1".into(),
        address: "10.0.0.1:9090".into(),
        node_type: "PEER".into(),
        is_active: true,
        capabilities: vec!["CORE".into()],
        registry_checksum: 0,
        latency_ms: 0,
        uptime_secs: 0,
        trust_score: 0.9,
        capability_bloom: CapabilityBloom::from_tokens(["status", "version"]),
        admission: PeerAdmission::Discovered,
        last_seen_secs: now_secs(),
        pubkey: String::new(),
        key_bound_at: 0,
        bind_sig: String::new(),
    };

    let capability_match = ClusterPeerNode {
        node_id: "n2".into(),
        address: "10.0.0.2:9090".into(),
        node_type: "PEER".into(),
        is_active: true,
        capabilities: vec!["CORE".into()],
        registry_checksum: 0,
        latency_ms: 0,
        uptime_secs: 0,
        trust_score: 0.6,
        capability_bloom: CapabilityBloom::from_tokens(["bloat_audit"]),
        admission: PeerAdmission::Discovered,
        last_seen_secs: now_secs(),
        pubkey: String::new(),
        key_bound_at: 0,
        bind_sig: String::new(),
    };

    let a_score = SusiSupervisor::score_peer_for_goal(&generic_high_trust, &goal_tokens);
    let b_score = SusiSupervisor::score_peer_for_goal(&capability_match, &goal_tokens);
    assert!(
        b_score > a_score,
        "peer with matching capability ({}) should outrank higher-trust peer with no match ({})",
        b_score,
        a_score
    );
}

#[test]
fn discovered_peers_never_receive_host_bearer() {
    // Local master is rostered as PeerAdmission::Local on the GMCP port.
    // The roster's local entry carries the instance's offset GMCP port.
    let local = format!(
        "127.0.0.1:{}",
        susi_paths::ports::effective(susi_paths::ports::GMCP)
    );
    assert!(SusiSupervisor::peer_allows_bearer(&local));
    // Arbitrary loopback ports are not automatic trust — a local listener
    // must not steal the host bearer just by binding nearby.
    assert!(!SusiSupervisor::peer_allows_bearer("127.0.0.1:19999"));
    assert!(!SusiSupervisor::peer_allows_bearer("localhost:9093"));
    // Any non-roster / Discovered address must be denied.
    assert!(!SusiSupervisor::peer_allows_bearer("10.0.0.99:9093"));
    assert!(!SusiSupervisor::peer_allows_bearer("192.168.1.50:9090"));
}

#[test]
fn test_consensus_master_bypasses_synthesis_for_single_valid_output() {
    let blackboard = Arc::new(susi_core::agent_types::HighDensityContextStore::default());
    blackboard.insert(
        "Qwen2ReasoningAgent".to_string(),
        "Photosynthesis is the process by which plants turn sunlight into energy.".to_string(),
    );

    let valid_outputs: Vec<(String, String)> = blackboard
        .iter()
        .into_iter()
        .filter_map(|r| {
            let agent_name = r.0.clone();
            let output = r.1.trim().to_string();
            if susi_gawd_agents::accountability::is_usable(&output) {
                Some((agent_name, output))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(valid_outputs.len(), 1);
    assert_eq!(
        valid_outputs[0].1,
        "Photosynthesis is the process by which plants turn sunlight into energy."
    );
}

#[test]
fn test_dominant_rank_leader_prefers_clear_outlier_over_llm_resynthesis() {
    let valid_outputs = vec![
        (
            "DevOpsAgent".to_string(),
            "# SUSI Bloat & Security Audit\n- Files Scanned: 54".to_string(),
        ),
        (
            "UniversalReasoner".to_string(),
            "I'm ready to code review Susi.".to_string(),
        ),
    ];
    let fleet_info = vec![
        GawdAgentInfo {
            name: "DevOpsAgent".to_string(),
            provider: "SUSI Local".into(),
            url: "native://substrate".into(),
            rank: 1.0,
        },
        GawdAgentInfo {
            name: "UniversalReasoner".to_string(),
            provider: "SUSI Local".into(),
            url: "native://substrate".into(),
            rank: 0.7,
        },
    ];

    let leader = SusiSupervisor::dominant_rank_leader(&valid_outputs, &fleet_info);
    assert_eq!(
        leader,
        Some("# SUSI Bloat & Security Audit\n- Files Scanned: 54")
    );
}

#[test]
fn test_dominant_rank_leader_defers_to_llm_synthesis_when_ranks_are_close() {
    let valid_outputs = vec![
        ("AgentA".to_string(), "Answer A".to_string()),
        ("AgentB".to_string(), "Answer B".to_string()),
    ];
    let fleet_info = vec![
        GawdAgentInfo {
            name: "AgentA".to_string(),
            provider: "SUSI Local".into(),
            url: "native://substrate".into(),
            rank: 0.9,
        },
        GawdAgentInfo {
            name: "AgentB".to_string(),
            provider: "SUSI Local".into(),
            url: "native://substrate".into(),
            rank: 0.85,
        },
    ];

    assert_eq!(
        SusiSupervisor::dominant_rank_leader(&valid_outputs, &fleet_info),
        None
    );
}

#[test]
fn test_unverified_text_does_not_increase_rank() {
    let (delta, source) = SusiSupervisor::rank_delta_for_output("Hardware Saturated: 8 CPUs.");
    assert_eq!(delta, 0.0);
    assert_eq!(source, "UNVERIFIED_OUTPUT");
}

#[test]
fn test_rank_delta_penalizes_self_reported_failure() {
    let (delta, source) =
        SusiSupervisor::rank_delta_for_output("Agent Execution Reported: FAILURE - timeout");
    assert_eq!(delta, -0.05);
    assert_eq!(source, "MISSION_FAILURE");
}

#[test]
fn test_rank_delta_penalizes_capability_gap() {
    let (delta, source) =
        SusiSupervisor::rank_delta_for_output("[CAPABILITY_GAP] Tool 'x' missing.");
    assert_eq!(delta, -0.05);
    assert_eq!(source, "MISSION_FAILURE");
}

#[test]
fn test_rank_delta_penalty_outweighs_reward() {
    // Deliberate asymmetry: trust should be harder to earn back than to
    // lose, so a single failure must undo more than a single success grants.
    let (reward, _) = SusiSupervisor::rank_delta_for_output("all clear");
    let (penalty, _) = SusiSupervisor::rank_delta_for_output("FAILURE: crashed");
    assert!(penalty.abs() > reward.abs());
}

#[test]
fn peer_scout_never_steals_host_contract_udp_port() {
    // While the host-contract discovery port is held (as the daemon would),
    // listing cluster nodes must still succeed — scouts bind ephemeral ports.
    // If the live daemon already owns 9092, that is the same precondition.
    let _holder =
        std::net::UdpSocket::bind(format!("127.0.0.1:{}", susi_paths::ports::UDP_DISCOVERY)).ok();
    let nodes = SusiSupervisor::list_cluster_nodes();
    assert!(
        nodes
            .iter()
            .any(|n| matches!(n.admission, PeerAdmission::Local)),
        "local master must remain discoverable without binding 9092"
    );
}

#[test]
fn signed_lan_discovery_frame_is_bound_to_its_route_and_never_exposes_host_token() {
    let dest: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
    let (frame, nonce) = super::signed_lan_discovery_frame("CORE,gmcp_http=9093", 42, "0", dest)
        .expect("cluster.key must produce a bound discovery ping");
    assert!(frame.starts_with("SUSI_PING_SIG3:"), "{frame}");
    assert!(!frame.contains("SUSI_LAN_PING"));
    let src: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let verified = crate::susi_config::cluster_key::verify_bound_ping(&frame, src)
        .expect("bound discovery frame must verify from its route source");
    assert_eq!(verified.nonce, nonce);
    assert_eq!(
        (verified.caps_csv.as_str(), verified.checksum),
        ("CORE,gmcp_http_9093", 42)
    );
    let elsewhere: std::net::SocketAddr = "10.9.9.9:40000".parse().unwrap();
    assert!(crate::susi_config::cluster_key::verify_bound_ping(&frame, elsewhere).is_err());
}
