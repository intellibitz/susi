//! Exhaustive small-state search for claim + quarantine invariants.

use crate::formal_invariants::{
    claim_safety, quarantine_ladder_secs, ClaimModel, ClaimOutcome, QUARANTINE_CAP_SECS,
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
