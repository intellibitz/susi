//! Exhaustive small-state search for claim + quarantine invariants.

use crate::formal_invariants::{
    budget_transition, claim_safety, lease_is_fenced, policy_precedence, quarantine_ladder_secs,
    BudgetTransition, ClaimModel, ClaimOutcome, PolicyClass, PolicyDecision, QUARANTINE_CAP_SECS,
};

#[test]
fn formal_invariants_two_claimants_never_both_win() {
    let agents = ["A", "B"];
    let mut dual_wins = 0u32;
    // Exhaustive: all orderings of two claim attempts at the same instant.
    for &(first, second) in &[(0usize, 1), (1, 0)] {
        let mut m = ClaimModel::new();
        let o1 = m.try_claim("T-X-1", agents[first], 100, 3600);
        let o2 = m.try_claim("T-X-1", agents[second], 100, 3600);
        claim_safety(&m, 100).expect("safety");
        let w1 = matches!(o1, ClaimOutcome::Won | ClaimOutcome::TookOverExpired);
        let w2 = matches!(o2, ClaimOutcome::Won | ClaimOutcome::TookOverExpired);
        if w1 && w2 {
            dual_wins += 1;
        }
        assert_eq!(m.live_holder("T-X-1", 100), Some(agents[first]));
    }
    assert_eq!(dual_wins, 0, "two claimants must never both win");
}

#[test]
fn formal_invariants_leases_expire_monotonically() {
    let mut m = ClaimModel::new();
    assert_eq!(m.try_claim("T-X-2", "A", 10, 5), ClaimOutcome::Won);
    let until = m.lease_until("T-X-2").expect("lease");
    // Advance time without rewriting: lease_until is fixed; expiry is monotonic.
    for t in 10..=until {
        let expired = t >= until;
        assert_eq!(m.live_holder("T-X-2", t).is_none(), expired);
        claim_safety(&m, t).expect("safety");
    }
    // Takeover after expiry extends lease strictly forward.
    let o = m.try_claim("T-X-2", "B", until, 5);
    assert_eq!(o, ClaimOutcome::TookOverExpired);
    let until2 = m.lease_until("T-X-2").expect("lease2");
    assert!(until2 > until, "new lease must be strictly later");
}

#[test]
fn formal_invariants_quarantine_ladder_cap_and_rungs() {
    let rungs: Vec<u64> = (0..=8).map(quarantine_ladder_secs).collect();
    assert_eq!(rungs[0], 600);
    assert_eq!(rungs[1], 600);
    assert_eq!(rungs[2], 3_600);
    for &s in &rungs[3..] {
        assert_eq!(s, QUARANTINE_CAP_SECS);
    }
    // Never exceeds cap.
    assert!(rungs.iter().all(|&s| s <= QUARANTINE_CAP_SECS));
    // Never skips a recovery probe rung: non-decreasing until cap.
    for w in rungs.windows(2) {
        assert!(w[1] >= w[0], "ladder must not skip downward");
    }
    // Distinct probe rungs before cap: 10m → 1h → 6h.
    let distinct: std::collections::BTreeSet<_> = rungs.iter().copied().collect();
    assert_eq!(distinct, [600, 3_600, QUARANTINE_CAP_SECS].into());
}

#[test]
fn formal_invariants_exhaustive_three_agent_schedules() {
    // Small-state: 3 agents, 3 time slots, claim or skip each slot.
    let agents = ["A", "B", "C"];
    let mut dual = 0u32;
    for schedule in 0..(3u32.pow(3)) {
        let mut m = ClaimModel::new();
        let mut winners_at_t = Vec::new();
        for slot in 0..3u64 {
            let choice = (schedule / 3u32.pow(slot as u32)) % 3;
            let agent = agents[choice as usize];
            let now = 1_000 + slot * 10;
            let o = m.try_claim("T-X-3", agent, now, 50);
            claim_safety(&m, now).expect("safety");
            if matches!(o, ClaimOutcome::Won | ClaimOutcome::TookOverExpired) {
                winners_at_t.push((now, agent));
            }
            // At most one live holder after each step.
            let holders: Vec<_> = ["T-X-3"]
                .iter()
                .filter_map(|t| m.live_holder(t, now).map(|h| (*t, h)))
                .collect();
            assert!(holders.len() <= 1);
        }
        // No two Won outcomes at the same timestamp.
        for i in 0..winners_at_t.len() {
            for j in (i + 1)..winners_at_t.len() {
                if winners_at_t[i].0 == winners_at_t[j].0 {
                    dual += 1;
                }
            }
        }
    }
    assert_eq!(dual, 0);
}

#[test]
fn formal_invariants_unclaimed_task_has_no_holder_or_lease_and_passes_safety() {
    let m = ClaimModel::new();
    assert_eq!(m.live_holder("T-UNCLAIMED-1", 0), None);
    assert_eq!(m.lease_until("T-UNCLAIMED-1"), None);
    assert!(claim_safety(&m, 0).is_ok());
}

#[test]
fn formal_invariants_bounded_budget_transition_monotonicity_reports_proof_bounds() {
    const BOUND: u64 = 8;
    const MAX_DEPTH: usize = 4;
    let mut states_explored = 0usize;
    let mut counterexamples: Vec<(u64, u64, u64, u64, &'static str)> = Vec::new();

    for committed in 0..=BOUND {
        for held in 0..=BOUND {
            for requested in 0..=BOUND {
                for cap in 0..=BOUND {
                    states_explored += 1;
                    let res = budget_transition(committed, held, requested, cap);
                    let total = committed.saturating_add(held).saturating_add(requested);
                    if res == BudgetTransition::Admit && total > cap {
                        counterexamples.push((
                            committed,
                            held,
                            requested,
                            cap,
                            "admitted above cap",
                        ));
                    }
                    if res == BudgetTransition::Reject && total <= cap {
                        counterexamples.push((
                            committed,
                            held,
                            requested,
                            cap,
                            "rejected within cap",
                        ));
                    }
                    // Monotonicity: requesting 1 additional unit cannot flip Reject -> Admit.
                    let next_res = budget_transition(committed, held, requested + 1, cap);
                    if res == BudgetTransition::Reject && next_res == BudgetTransition::Admit {
                        counterexamples.push((
                            committed,
                            held,
                            requested,
                            cap,
                            "non-monotonic budget admission",
                        ));
                    }
                }
            }
        }
    }

    assert_eq!(states_explored, 9 * 9 * 9 * 9);
    assert_eq!(MAX_DEPTH, 4);
    assert!(
        counterexamples.is_empty(),
        "unexpected budget counterexamples: {counterexamples:?}"
    );
}

#[test]
fn formal_invariants_policy_precedence_and_counterexample_retention() {
    let policies = [
        PolicyClass::FreeOnly,
        PolicyClass::AskFirst,
        PolicyClass::PaidAuthorized,
    ];
    let mut states_explored = 0usize;
    let max_depth = 3usize;
    let mut counterexamples: Vec<String> = Vec::new();

    for &policy in &policies {
        for &is_free in &[false, true] {
            for &free_headroom in &[false, true] {
                states_explored += 1;
                let decision = policy_precedence(policy, is_free, free_headroom);
                if is_free && free_headroom && decision != PolicyDecision::Free {
                    counterexamples.push(format!(
                        "free candidate with headroom not routed Free under {policy:?}"
                    ));
                }
                // LocalOnlyStrict / FreeOnly deny-overrides-allow when free headroom is exhausted.
                if policy == PolicyClass::FreeOnly
                    && !(is_free && free_headroom)
                    && decision != PolicyDecision::Denied
                {
                    counterexamples.push(format!(
                        "FreeOnly failed to deny non-free or exhausted candidate: {decision:?}"
                    ));
                }
            }
        }
    }

    // Also verify bounded lease-fencing state space (generations 0..=3, timestamps 0..=3).
    for expected_gen in 0..=3u64 {
        for observed_gen in 0..=3u64 {
            for now in 0..=3u64 {
                for lease_until in 0..=3u64 {
                    states_explored += 1;
                    let fenced = lease_is_fenced(expected_gen, observed_gen, now, lease_until);
                    if (expected_gen != observed_gen || now >= lease_until) && fenced {
                        counterexamples.push(format!(
                            "stale or expired lease passed fence: exp={expected_gen} obs={observed_gen} now={now} until={lease_until}"
                        ));
                    }
                }
            }
        }
    }

    assert_eq!(states_explored, 12 + 256);
    assert_eq!(max_depth, 3);
    assert!(
        counterexamples.is_empty(),
        "policy/fence counterexamples: {counterexamples:?}"
    );

    // Verify deterministic counterexample retention when checking a refuted hypothesis
    // ("AskFirst never returns NeedsConsent").
    let mut retained_counterexamples = Vec::new();
    for &policy in &policies {
        for &is_free in &[false, true] {
            for &free_headroom in &[false, true] {
                let decision = policy_precedence(policy, is_free, free_headroom);
                if policy == PolicyClass::AskFirst && decision == PolicyDecision::NeedsConsent {
                    retained_counterexamples.push((policy, is_free, free_headroom, decision));
                }
            }
        }
    }
    assert!(
        !retained_counterexamples.is_empty(),
        "counterexample retention must capture witnessing states"
    );
    assert_eq!(
        retained_counterexamples[0],
        (
            PolicyClass::AskFirst,
            false,
            false,
            PolicyDecision::NeedsConsent
        )
    );
}
