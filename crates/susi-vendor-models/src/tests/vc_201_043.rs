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

#[test]
fn vc_201_043_release_restores_capacity_for_subsequent_admission() {
    let mut p = AccelPool {
        total_bytes: 8_000,
        reserved_bytes: 0,
    };
    assert_eq!(
        p.admit(3_000),
        AdmitDecision::Admitted { reservation: 3_000 }
    );
    p.release(3_000);
    assert_eq!(p.reserved_bytes, 0);
    assert_eq!(p.free(), 8_000);
    assert_eq!(
        p.admit(7_600),
        AdmitDecision::Admitted { reservation: 7_600 }
    );
    assert_eq!(p.reserved_bytes, 7_600);
}

#[test]
fn vc_201_043_zero_byte_request_is_rejected_without_mutating_pool() {
    let mut p = AccelPool {
        total_bytes: 4_000,
        reserved_bytes: 500,
    };
    assert_eq!(p.admit(0), AdmitDecision::RejectedInsufficient);
    assert_eq!(p.reserved_bytes, 500);
}

#[test]
fn vc_201_043_free_saturates_at_zero_when_reserved_exceeds_total() {
    let mut p = AccelPool {
        total_bytes: 1_000,
        reserved_bytes: 1_200,
    };
    assert_eq!(p.free(), 0);
    assert_eq!(p.admit(1), AdmitDecision::RejectedInsufficient);
}
