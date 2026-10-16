#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-046: Bounded inference batching for capable backends.
//!
//! Mastery criteria:
//! 1. Bounded batching only for backends that support it; unsupported backends
//!    are refused at construction time.
//! 2. Per-request deadlines and cancellation: cancelled or expired requests
//!    never reach the backend execution pipeline, and are answered with
//!    explicit `Cancelled` or `DeadlineExceeded` outcomes.
//! 3. Latency limits: `max_wait_ms` ensures older enqueued requests are not
//!    starved waiting for a full batch.
//! 4. Memory/queue bounds: queue depth is strictly bounded by `max_queue`,
//!    rejecting excess requests immediately before memory exhaustion.
//! 5. Load tests demonstrate throughput changes: multiple requests collapse
//!    into a minimal number of backend batch invocations without violating
//!    configured latency or memory limits.

use std::cell::Cell;
use susi_vendor_models::infer_batch::{
    BatchBackend, BatchLimits, BatchOutcome, Batcher, EngineBatchBackend, Request,
};

struct MockBackend {
    supported: bool,
    invocations: Cell<usize>,
    executed_prompts: Cell<usize>,
}

impl MockBackend {
    fn new(supported: bool) -> Self {
        Self {
            supported,
            invocations: Cell::new(0),
            executed_prompts: Cell::new(0),
        }
    }
}

impl BatchBackend for MockBackend {
    fn supports_batch(&self) -> bool {
        self.supported
    }

    fn run(&self, prompts: &[String]) -> Vec<String> {
        self.invocations.set(self.invocations.get() + 1);
        self.executed_prompts
            .set(self.executed_prompts.get() + prompts.len());
        prompts.iter().map(|p| format!("response:{p}")).collect()
    }
}

#[test]
fn vc_201_046_mastery() {
    // 1. Unsupported backend refused at construction
    let unsupp = MockBackend::new(false);
    let limits = BatchLimits {
        max_batch: 8,
        max_queue: 32,
        max_wait_ms: 50,
    };
    let res = Batcher::new(limits.clone(), &unsupp);
    assert!(res.is_err(), "Unsupported backend must be rejected");

    // 2. Cancellation and deadlines honored on production engine backend
    let engine = EngineBatchBackend::new(|prompts: &[String]| {
        prompts.iter().map(|p| format!("engine:{p}")).collect()
    });
    let mut batcher = Batcher::new(limits.clone(), &engine).unwrap();

    let now = 1_000u64;
    // Normal request
    batcher
        .submit(
            Request {
                id: 101,
                prompt: "q1".into(),
                deadline_ms: now + 500,
            },
            now,
        )
        .unwrap();

    // Cancelled request
    batcher
        .submit(
            Request {
                id: 102,
                prompt: "to_cancel".into(),
                deadline_ms: now + 500,
            },
            now,
        )
        .unwrap();
    batcher.cancel(102).unwrap();

    // Already expired request
    batcher
        .submit(
            Request {
                id: 103,
                prompt: "already_expired".into(),
                deadline_ms: now - 10,
            },
            now,
        )
        .unwrap();

    // Flush at now + 20
    let flush_res = batcher.flush(now + 20);
    assert!(
        flush_res
            .iter()
            .any(|(id, o)| *id == 102 && *o == BatchOutcome::Cancelled),
        "Cancelled request must yield Cancelled outcome"
    );
    assert_eq!(batcher.outcome(102), Some(&BatchOutcome::Cancelled));
    assert_eq!(batcher.outcome(103), Some(&BatchOutcome::DeadlineExceeded));
    assert!(matches!(
        batcher.outcome(101),
        Some(BatchOutcome::Done { output }) if output == "engine:q1"
    ));
    assert_eq!(
        engine.invocations(),
        1,
        "Engine backend was called exactly once for valid prompts"
    );

    // 3. Queue bound prevents unbounded memory growth
    let queue_limits = BatchLimits {
        max_batch: 4,
        max_queue: 3,
        max_wait_ms: 100,
    };
    let backend = MockBackend::new(true);
    let mut q_batcher = Batcher::new(queue_limits, &backend).unwrap();
    q_batcher
        .submit(
            Request {
                id: 1,
                prompt: "a".into(),
                deadline_ms: now + 1000,
            },
            now,
        )
        .unwrap();
    q_batcher
        .submit(
            Request {
                id: 2,
                prompt: "b".into(),
                deadline_ms: now + 1000,
            },
            now,
        )
        .unwrap();
    q_batcher
        .submit(
            Request {
                id: 3,
                prompt: "c".into(),
                deadline_ms: now + 1000,
            },
            now,
        )
        .unwrap();
    assert!(
        q_batcher
            .submit(
                Request {
                    id: 4,
                    prompt: "overflow".into(),
                    deadline_ms: now + 1000,
                },
                now,
            )
            .is_err(),
        "Submissions beyond max_queue must be refused to bound memory"
    );

    // 4. Latency bound: max_wait flushes before deadline even when batch is incomplete
    assert!(!q_batcher.should_flush(now + 40));
    assert!(
        q_batcher.should_flush(now + 120),
        "Batcher must signal flush when oldest request exceeds max_wait_ms"
    );
    let out = q_batcher.flush(now + 120);
    assert_eq!(out.len(), 3);
    assert_eq!(backend.invocations.get(), 1);

    // 5. Load test: demonstrate significant throughput win
    // 50 requests collapsed into bounded batches of 10
    let load_limits = BatchLimits {
        max_batch: 10,
        max_queue: 100,
        max_wait_ms: 200,
    };
    let load_backend = MockBackend::new(true);
    let mut load_batcher = Batcher::new(load_limits, &load_backend).unwrap();

    let total_requests = 50;
    for i in 1..=total_requests {
        load_batcher
            .submit(
                Request {
                    id: i,
                    prompt: format!("load_prompt_{i}"),
                    deadline_ms: now + 5000,
                },
                now,
            )
            .unwrap();
    }

    // Process all requests in batches
    let mut flushed_total = 0;
    let mut batch_count = 0;
    while load_batcher.pending_count() > 0 {
        assert!(
            load_batcher.should_flush(now),
            "Should flush as batch limit is reached"
        );
        let batch_out = load_batcher.flush(now);
        flushed_total += batch_out.len();
        batch_count += 1;
    }

    assert_eq!(flushed_total, total_requests as usize);
    assert_eq!(
        batch_count, 5,
        "50 requests must be processed in exactly 5 batches of 10"
    );
    assert_eq!(
        load_backend.invocations.get(),
        5,
        "Backend invocations reduced 10x (from 50 single calls to 5 batch calls)"
    );
    assert_eq!(
        load_backend.executed_prompts.get(),
        50,
        "All 50 prompts executed successfully"
    );
}
