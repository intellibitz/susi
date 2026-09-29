use crate::accel_reserve::{AccelPool, AdmitDecision};

#[test]
fn vc_201_043_reserves_before_admission() {
    let mut p = AccelPool {
        total_bytes: 8_000,
        reserved_bytes: 0,
    };
    match p.admit(3_000) {
        AdmitDecision::Admitted { reservation } => assert_eq!(reservation, 3_000),
        AdmitDecision::RejectedInsufficient => panic!("should admit"),
    }
    assert_eq!(p.reserved_bytes, 3_000);
}

#[test]
fn vc_201_043_rejects_when_insufficient() {
    let mut p = AccelPool {
        total_bytes: 1000,
        reserved_bytes: 800,
    };
    assert_eq!(p.admit(300), AdmitDecision::RejectedInsufficient);
    assert_eq!(p.reserved_bytes, 800);
}
