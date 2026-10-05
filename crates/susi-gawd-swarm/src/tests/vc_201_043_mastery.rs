//! Mastery verification for VC-201-043: model loads and inference
//! reservations are coordinated against *measured* accelerator headroom —
//! the production admission ledger derives local capacity from the host,
//! VRAM runs through `AccelPool`'s reserve rule, concurrent reservations
//! release deterministically, and an unmeasurable accelerator fails
//! closed rather than overcommitting phantom capacity.

use crate::parallel_admission::{
    AdmissionController, AdmissionLimits, AdmitRequest, Backpressure, LocalCapacity,
};
use std::sync::Mutex;

/// Serializes tests that read or mutate `SUSI_ADMIT_VRAM_MB`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn req_vram<'a>(mission: &'a str, job: &'a str, vram_mb: u32) -> AdmitRequest<'a> {
    AdmitRequest {
        mission,
        job,
        scopes: Vec::new(),
        cpu_millis: 0,
        ram_mb: 0,
        vram_mb,
        subprocesses: 0,
        holds_job_slot: true,
    }
}

fn vram_only(vram_mb: u32) -> AdmissionLimits {
    AdmissionLimits {
        local: LocalCapacity {
            cpu_millis: u32::MAX,
            ram_mb: u32::MAX,
            vram_mb,
            subprocesses: u32::MAX,
        },
        ..Default::default()
    }
}

/// The production limits derive `local` from this host: CPU parallelism
/// and subprocess slots track `available_parallelism`, RAM tracks
/// `/proc/meminfo` MemAvailable (or stays ungated when unmeasurable), and
/// VRAM tracks the accelerator probe or the operator escape — never the
/// old baked-in 64 GiB assumption.
#[test]
fn vc_201_043_mastery_measured_limits_derive_from_the_host() {
    let _env = ENV_LOCK.lock().unwrap();
    std::env::remove_var("SUSI_ADMIT_VRAM_MB");
    let lim = AdmissionLimits::measured();
    let parallelism = std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(1);
    if std::env::var("SUSI_ADMIT_CPU_MILLIS").is_err() {
        assert_eq!(lim.local.cpu_millis, (parallelism * 1000) as u32);
    }
    assert_eq!(lim.local.subprocesses, parallelism as u32);
    // Host figures drift between the two probes this test and measured()
    // each run — the pin is "derived from the host, not the 64/256 GiB
    // phantoms", so a ±8 GiB window is the honest bound.
    if std::env::var("SUSI_ADMIT_RAM_MB").is_err() {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            if let Some(kib) = meminfo
                .lines()
                .find_map(|l| l.strip_prefix("MemAvailable:"))
                .and_then(|r| r.split_whitespace().next())
                .and_then(|v| v.parse::<u64>().ok())
            {
                let want = (kib / 1024).min(u32::MAX as u64);
                assert!(
                    u64::from(lim.local.ram_mb).abs_diff(want) <= 8192,
                    "ram_mb {} should track MemAvailable {}",
                    lim.local.ram_mb,
                    want
                );
            }
        }
    }
    let probed = susi_vendor_models::accel_reserve::probe_vram_free_mb();
    if let Some(free) = probed {
        let want = free.min(u32::MAX as u64);
        assert!(
            u64::from(lim.local.vram_mb).abs_diff(want) <= 8192,
            "vram_mb {} should track the probe {}",
            lim.local.vram_mb,
            want
        );
    } else {
        assert_eq!(lim.local.vram_mb, 0, "unmeasured VRAM fails closed");
    }
}

/// The operator escape is honored: `SUSI_ADMIT_VRAM_MB` overrides the
/// probe for containers and CI where nvidia-smi is unavailable or lies.
#[test]
fn vc_201_043_mastery_operator_vram_override_wins() {
    let _env = ENV_LOCK.lock().unwrap();
    std::env::set_var("SUSI_ADMIT_VRAM_MB", "12288");
    let lim = AdmissionLimits::measured();
    std::env::remove_var("SUSI_ADMIT_VRAM_MB");
    assert_eq!(lim.local.vram_mb, 12_288);
}

/// An unmeasurable accelerator fails closed: with measured VRAM of 0 a
/// job that needs accelerator memory is refused instead of admitted
/// against an assumed 64 GiB — while CPU-only work still flows.
#[test]
fn vc_201_043_mastery_unmeasured_accelerator_fails_closed() {
    let ctrl = AdmissionController::new(vram_only(0));
    let denied = ctrl
        .admit(&req_vram("m", "gpu-job", 1024))
        .expect_err("a VRAM request on an unmeasured host must be refused");
    assert!(
        matches!(
            denied,
            Backpressure::LocalExhausted | Backpressure::Queued { .. }
        ),
        "expected a local-capacity denial, got {denied:?}"
    );
    // A job that needs no accelerator still admits on the same host —
    // as a nested (non-slot) admission it does not queue behind the
    // refused GPU job's held slot.
    let mut cpu_req = req_vram("m", "cpu-job", 0);
    cpu_req.holds_job_slot = false;
    ctrl.admit(&cpu_req)
        .expect("CPU-only work must not be gated by missing VRAM")
        .release();
}

/// VRAM admission runs through `AccelPool`'s headroom rule: a request
/// that would leave less than the pool's reserve fraction free is
/// refused even though raw capacity would fit it — the control plane
/// keeps accelerator room under full model admission.
#[test]
fn vc_201_043_mastery_headroom_reserve_is_preserved() {
    // 10 GiB measured; the pool's reserve is 5% = 512 MiB.
    let ctrl = AdmissionController::new(vram_only(10 * 1024));
    ctrl.admit(&req_vram("m", "load-a", 9 * 1024))
        .expect("9216 MiB fits and leaves 1 GiB above the reserve")
        .release();
    let denied = ctrl
        .admit(&req_vram("m", "load-b", 10 * 1024))
        .expect_err("filling the pool to 100% must be refused");
    assert!(
        matches!(
            denied,
            Backpressure::LocalExhausted | Backpressure::Queued { .. }
        ),
        "expected a capacity denial preserving headroom, got {denied:?}"
    );
}

/// Concurrent reservations never oversubscribe and always release: two
/// contenders split measured headroom, a third waits until a ticket is
/// dropped, and a persisted controller reloaded from disk keeps the
/// reservation a restarted process must honor.
#[test]
fn vc_201_043_mastery_concurrent_reservations_release_and_persist() {
    let dir = std::env::temp_dir().join(format!("susi-vc043-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("admission.json");

    // 10 GiB; two 4 GiB loads fit (free-after ≥ 512 MiB each), the third
    // would leave < reserve → waits.
    let ctrl = AdmissionController::persisted(path.clone(), vram_only(10 * 1024));
    let a = ctrl
        .admit(&req_vram("m", "a", 4 * 1024))
        .expect("first load admits");
    let b = ctrl
        .admit(&req_vram("m", "b", 4 * 1024))
        .expect("second load admits alongside the first");
    let denied = ctrl
        .probe(&req_vram("m", "c", 4 * 1024))
        .expect_err("a third concurrent load must wait for headroom");
    assert!(
        matches!(
            denied,
            Backpressure::LocalExhausted | Backpressure::Queued { .. }
        ),
        "expected queued/exhausted, got {denied:?}"
    );

    // A persisted reload sees the live reservations — a restarted
    // process cannot silently oversubscribe what `a` and `b` hold.
    let reloaded = AdmissionController::persisted(path.clone(), vram_only(10 * 1024));
    assert!(
        reloaded.probe(&req_vram("m", "c2", 4 * 1024)).is_err(),
        "the reloaded ledger must still count the held VRAM"
    );

    // Releasing a reservation frees headroom for the waiting load.
    a.release();
    ctrl.admit(&req_vram("m", "c2", 4 * 1024))
        .expect("the queued load admits once a ticket releases")
        .release();
    b.release();

    let _ = std::fs::remove_dir_all(&dir);
}
