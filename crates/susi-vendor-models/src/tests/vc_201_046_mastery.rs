//! Vector VC-201-046 mastery tests.
//!
//! Vector: Batch compatible local inference requests.
//! Mastery target: Introduce bounded batching only for backends that support it,
//! with per-request deadlines and cancellation; load tests demonstrate
//! throughput changes without violating configured latency or memory limits.

use crate::infer_batch::{BatchBackend, BatchLimits, BatchOutcome, Batcher, Request};
use std::cell::Cell;

struct TestBackend {
    supported: bool,
    invocations: Cell<usize>,
    batch_sizes: Cell<Vec<usize>>,
}

impl TestBackend {
    fn new(supported: bool) -> Self {
        Self {
            supported,
            invocations: Cell::new(0),
            batch_sizes: Cell::new(Vec::new()),
        }
    }
}

impl BatchBackend for TestBackend {
    fn supports_batch(&self) -> bool {
        self.supported
    }

    fn run(&self, prompts: &[String]) -> Vec<String> {
        self.invocations.set(self.invocations.get() + 1);
        let mut sizes = self.batch_sizes.take();
        sizes.push(prompts.len());
        self.batch_sizes.set(sizes);
        prompts.iter().map(|p| format!("res:{p}")).collect()
    }
}

fn test_limits(max_batch: usize, max_queue: usize, max_wait_ms: u64) -> BatchLimits {
    BatchLimits {
        max_batch,
        max_queue,
        max_wait_ms,
    }
}

#[test]
fn vc_201_046_mastery_unsupported_backend_refused_at_construction() {
    let unsupp = TestBackend::new(false);
    let res = Batcher::new(test_limits(4, 16, 100), &unsupp);
    assert!(res.is_err());
    let err_msg = res.err().unwrap().to_string();
    assert!(err_msg.contains("does not support batching"));
}

#[test]
fn vc_201_046_mastery_cancellation_and_deadline_exceeded_never_run_late() {
    let backend = TestBackend::new(true);
    let mut batcher = Batcher::new(test_limits(4, 16, 100), &backend).unwrap();

    let now = 1000u64;

    // Normal request
    let req1 = Request {
        id: 1,
        prompt: "hello".into(),
        deadline_ms: now + 500,
    };
    batcher.submit(req1, now).unwrap();

    // Cancelled request
    let req2 = Request {
        id: 2,
        prompt: "to_cancel".into(),
        deadline_ms: now + 500,
    };
    batcher.submit(req2, now).unwrap();
    batcher.cancel(2).unwrap();

    // Already expired request
    let req3 = Request {
        id: 3,
        prompt: "expired".into(),
        deadline_ms: now - 10,
    };
    batcher.submit(req3, now).unwrap();

    // Flush at now + 50 (within req1 deadline, but after req3 deadline)
    let outcomes = batcher.flush(now + 50);

    // Cancelled request should record Cancelled outcome
    assert_eq!(outcomes.get(&2), Some(&BatchOutcome::Cancelled));
    // Expired request should record DeadlineExceeded outcome
    assert_eq!(outcomes.get(&3), Some(&BatchOutcome::DeadlineExceeded));
    // Normal request should have executed
    assert_eq!(
        outcomes.get(&1),
        Some(&BatchOutcome::Done {
            output: "res:hello".into()
        })
    );

    // Backend only saw the 1 valid prompt, not the cancelled or expired ones
    assert_eq!(backend.invocations.get(), 1);
    assert_eq!(backend.batch_sizes.take(), vec![1]);
}
