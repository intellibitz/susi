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
