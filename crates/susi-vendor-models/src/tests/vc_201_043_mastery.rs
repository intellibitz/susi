//! Vector VC-201-043 mastery tests.
//!
//! Vector: Reserve accelerator memory before model admission.
//! Mastery target: Coordinate model loads and inference reservations using
//! measured memory headroom; concurrent load and out-of-memory tests release
//! reservations and preserve a responsive control plane.

use crate::accel_reserve::{AccelPool, AdmitDecision};

#[test]
fn vc_201_043_mastery_reservation_admit_and_release_lifecycle() {
    let mut pool = AccelPool {
        total_bytes: 16 * 1024 * 1024 * 1024, // 16 GiB total accelerator
        reserved_bytes: 0,
    };

    // Before model admission, must reserve needed memory from available headroom
    let load_req_bytes = 10 * 1024 * 1024 * 1024; // 10 GiB
    match pool.admit(load_req_bytes) {
        AdmitDecision::Admitted { reservation } => {
            assert_eq!(reservation, load_req_bytes);
        }
        AdmitDecision::RejectedInsufficient => panic!("should admit when headroom is sufficient"),
    }
    assert_eq!(pool.reserved_bytes, load_req_bytes);
    assert_eq!(pool.free(), 6 * 1024 * 1024 * 1024);

    // Second admission exceeding headroom must be rejected
    let second_req_bytes = 8 * 1024 * 1024 * 1024; // 8 GiB > 6 GiB free
    assert_eq!(
        pool.admit(second_req_bytes),
        AdmitDecision::RejectedInsufficient
    );
    // Reserved bytes must not be corrupted by rejected attempt
    assert_eq!(pool.reserved_bytes, load_req_bytes);

    // Simulated load failure / out-of-memory release must restore headroom
    pool.release(load_req_bytes);
    assert_eq!(pool.reserved_bytes, 0);
    assert_eq!(pool.free(), 16 * 1024 * 1024 * 1024);
}

#[test]
fn vc_201_043_mastery_zero_or_negative_request_rejected() {
    let mut pool = AccelPool {
        total_bytes: 8 * 1024 * 1024 * 1024,
        reserved_bytes: 0,
    };

    // Zero byte reservation is invalid
    assert_eq!(pool.admit(0), AdmitDecision::RejectedInsufficient);
    assert_eq!(pool.reserved_bytes, 0);
}
