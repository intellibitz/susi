use crate::accel_reserve::{AccelPool, AdmitDecision};

#[test]
fn vc_201_043_headroom_is_5_percent() {
    let p = AccelPool {
        total_bytes: 1000,
        reserved_bytes: 0,
    };
    assert_eq!(p.headroom(), 50);
}

#[test]
fn vc_201_043_accel_headroom_reserved_on_runtime_path() {
    let mut p = AccelPool {
        total_bytes: 1000,
        reserved_bytes: 900,
    };
    // free is 100, headroom is 50.
    // need 50 -> free - need = 50 >= 50 (headroom) -> should admit
    match p.admit(50) {
        AdmitDecision::Admitted { reservation } => assert_eq!(reservation, 50),
        AdmitDecision::RejectedInsufficient => panic!("should admit"),
    }
    assert_eq!(p.reserved_bytes, 950);

    // free is 50, headroom is 50.
    // need 1 -> free - need = 49 < 50 (headroom) -> should reject
    assert_eq!(p.admit(1), AdmitDecision::RejectedInsufficient);
    assert_eq!(p.reserved_bytes, 950);
}

#[test]
fn vc_201_043_headroom_scales_linearly_across_pool_sizes() {
    for (total_bytes, expected_headroom) in [(0, 0), (19, 0), (20, 1), (20_000, 1_000)] {
        let p = AccelPool {
            total_bytes,
            reserved_bytes: 0,
        };
        assert_eq!(p.headroom(), expected_headroom);
    }
}

#[test]
fn vc_201_043_headroom_preserved_across_multiple_sequential_admissions_and_releases() {
    let mut p = AccelPool {
        total_bytes: 2_000,
        reserved_bytes: 0,
    };
    assert_eq!(p.headroom(), 100);
    assert_eq!(
        p.admit(1_000),
        AdmitDecision::Admitted { reservation: 1_000 }
    );
    assert_eq!(p.admit(900), AdmitDecision::Admitted { reservation: 900 });
    assert_eq!(p.reserved_bytes, 1_900);
    assert_eq!(p.admit(1), AdmitDecision::RejectedInsufficient);

    p.release(500);
    assert_eq!(p.reserved_bytes, 1_400);
    assert_eq!(p.admit(500), AdmitDecision::Admitted { reservation: 500 });
    assert_eq!(p.reserved_bytes, 1_900);
}
