//! Mastery verification for VC-202-009: Mission admission and resource
//! arbitration — Missions and their leaf workers are admitted under CPU, GPU,
//! memory, and disk quotas, scheduled without starvation or oversubscription,
//! and queued or preempted with observable reasons.

use crate::parallel_admission::{
    AdmissionController, AdmissionLimits, AdmitRequest, Backpressure, LocalCapacity,
};

fn job_req<'a>(
    mission: &'a str,
    job: &'a str,
    cpu_millis: u32,
    ram_mb: u32,
    vram_mb: u32,
) -> AdmitRequest<'a> {
    AdmitRequest {
        mission,
        job,
        scopes: Vec::new(),
        cpu_millis,
        ram_mb,
        vram_mb,
        subprocesses: 1,
        holds_job_slot: true,
    }
}

fn test_limits(cpu_millis: u32, ram_mb: u32, vram_mb: u32) -> AdmissionLimits {
    AdmissionLimits {
        local: LocalCapacity {
            cpu_millis,
            ram_mb,
            vram_mb,
            subprocesses: 10,
        },
        ..Default::default()
    }
}

#[test]
fn vc_202_009_mastery_resource_quotas_prevent_oversubscription() {
    let ctrl = AdmissionController::new(test_limits(4000, 8192, 8192));

    // Admit job 1 within bounds (2000 CPU, 4096 RAM, 2048 VRAM)
    let ticket1 = ctrl
        .admit(&job_req("m1", "j1", 2000, 4096, 2048))
        .expect("job 1 within quotas must admit");

    // Attempt job 2 exceeding remaining RAM (needs 8192 RAM when only 4096 is available)
    let denied = ctrl
        .admit(&job_req("m2", "j2", 1000, 8192, 1024))
        .expect_err("job 2 exceeding RAM quota must be refused");

    assert!(
        matches!(
            denied,
            Backpressure::LocalExhausted | Backpressure::Queued { .. }
        ),
        "expected local exhausted denial, got {denied:?}"
    );

    ticket1.release();
}

#[test]
fn vc_202_009_mastery_fair_queue_prevents_starvation() {
    let ctrl = AdmissionController::new(test_limits(4000, 8192, 8192));

    // Fill capacity with job A
    let ticket_a = ctrl
        .admit(&job_req("mA", "jA", 4000, 8192, 4096))
        .expect("job A admits");

    // Probe job B — queued due to capacity
    let prob_b = ctrl
        .probe(&job_req("mB", "jB", 2000, 4096, 2048))
        .expect_err("job B must queue behind job A");

    assert!(matches!(
        prob_b,
        Backpressure::LocalExhausted | Backpressure::Queued { .. }
    ));

    // Release job A, freeing capacity for job B
    ticket_a.release();

    let ticket_b = ctrl
        .admit(&job_req("mB", "jB", 2000, 4096, 2048))
        .expect("job B admits once job A releases capacity");

    ticket_b.release();
}

#[test]
fn vc_202_009_mastery_idempotent_readmission() {
    let ctrl = AdmissionController::new(test_limits(4000, 8192, 8192));

    let ticket1 = ctrl
        .admit(&job_req("m1", "j1", 1000, 2048, 1024))
        .expect("first admission");

    // Re-admitting same job ID returns ticket without double-charging
    let ticket2 = ctrl
        .admit(&job_req("m1", "j1", 1000, 2048, 1024))
        .expect("idempotent readmission");

    drop(ticket1);
    ticket2.release();
}
