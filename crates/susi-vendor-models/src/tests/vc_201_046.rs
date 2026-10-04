//! Bounded inference batching for capable backends (VC-201-046).

use crate::infer_batch::{
    BatchBackend, BatchLimits, BatchOutcome, Batcher, EngineBatchBackend, Request,
};
use std::cell::Cell;

struct Echo {
    supports: bool,
    calls: Cell<usize>,
    last_n: Cell<usize>,
}

impl BatchBackend for Echo {
    fn supports_batch(&self) -> bool {
        self.supports
    }
    fn run(&self, prompts: &[String]) -> Vec<String> {
        self.calls.set(self.calls.get() + 1);
        self.last_n.set(prompts.len());
        prompts.iter().map(|p| format!("out:{p}")).collect()
    }
}

fn limits() -> BatchLimits {
    BatchLimits {
        max_batch: 3,
        max_queue: 8,
        max_wait_ms: 100,
    }
}

fn req(id: u64, prompt: &str, deadline: u64) -> Request {
    Request {
        id,
        prompt: prompt.to_string(),
        deadline_ms: deadline,
    }
}

#[test]
fn vc_201_046_non_batch_backends_are_refused() {
    let b = Echo {
        supports: false,
        calls: Cell::new(0),
        last_n: Cell::new(0),
    };
    assert!(Batcher::new(limits(), &b).is_err());
}

#[test]
fn vc_201_046_full_batch_flushes_in_one_backend_call() {
    let b = Echo {
        supports: true,
        calls: Cell::new(0),
        last_n: Cell::new(0),
    };
    let mut batch = Batcher::new(limits(), &b).unwrap();
    for id in 0..3 {
        batch.submit(req(id, &format!("p{id}"), 1000), 0).unwrap();
    }
    assert!(batch.should_flush(0), "max_batch reached");
    let out = batch.flush(1);
    assert_eq!(out.len(), 3);
    assert_eq!(b.calls.get(), 1, "one batched backend call");
    assert_eq!(b.last_n.get(), 3);
    assert!(matches!(
        batch.outcome(2),
        Some(BatchOutcome::Done { output }) if output == "out:p2"
    ));
}

#[test]
fn vc_201_046_deadline_overrun_is_answered_not_run_late() {
    let b = Echo {
        supports: true,
        calls: Cell::new(0),
        last_n: Cell::new(0),
    };
    let mut batch = Batcher::new(limits(), &b).unwrap();
    batch.submit(req(1, "late", 50), 0).unwrap();
    batch.submit(req(2, "fine", 5000), 0).unwrap();
    let out = batch.flush(100); // req 1's deadline already passed
    assert!(out
        .iter()
        .any(|(id, o)| *id == 1 && *o == BatchOutcome::DeadlineExceeded));
    assert!(out
        .iter()
        .any(|(id, o)| matches!(o, BatchOutcome::Done { .. } if *id == 2)));
    assert_eq!(
        b.last_n.get(),
        1,
        "expired request never reaches the backend"
    );
}

#[test]
fn vc_201_046_cancelled_requests_do_not_run() {
    let b = Echo {
        supports: true,
        calls: Cell::new(0),
        last_n: Cell::new(0),
    };
    let mut batch = Batcher::new(limits(), &b).unwrap();
    batch.submit(req(1, "a", 1000), 0).unwrap();
    batch.submit(req(2, "b", 1000), 0).unwrap();
    batch.cancel(1).unwrap();
    let out = batch.flush(10);
    assert!(out
        .iter()
        .any(|(id, o)| *id == 1 && *o == BatchOutcome::Cancelled));
    assert_eq!(b.last_n.get(), 1);
}

#[test]
fn vc_201_046_queue_bound_and_wait_based_flush() {
    let b = Echo {
        supports: true,
        calls: Cell::new(0),
        last_n: Cell::new(0),
    };
    let l = BatchLimits {
        max_batch: 10,
        max_queue: 2,
        max_wait_ms: 100,
    };
    let mut batch = Batcher::new(l, &b).unwrap();
    batch.submit(req(1, "a", 10_000), 0).unwrap();
    batch.submit(req(2, "b", 10_000), 0).unwrap();
    assert!(batch.submit(req(3, "c", 10_000), 0).is_err(), "queue full");
    // Under max_batch but oldest waited past max_wait → flush anyway.
    assert!(!batch.should_flush(50));
    assert!(batch.should_flush(150));
    let out = batch.flush(150);
    assert_eq!(out.len(), 2);
}

/// Acceptance: the production [`EngineBatchBackend`] — not a test `Echo` —
/// is called by the [`Batcher`], and batching collapses many requests into
/// one engine invocation (the throughput win), while deadlines and
/// cancellation still hold on that production path.
#[test]
fn vc_201_046_batcher_called_by_a_production_backend() {
    let backend = EngineBatchBackend::new(|prompts: &[String]| {
        prompts.iter().map(|p| format!("out:{p}")).collect()
    });
    let mut batcher = Batcher::new(
        BatchLimits {
            max_batch: 8,
            max_queue: 8,
            max_wait_ms: 100,
        },
        &backend,
    )
    .unwrap();

    // Four live requests plus one cancelled plus one already-expired.
    for i in 1..=4u64 {
        batcher.submit(req(i, &format!("p{i}"), 10_000), 0).unwrap();
    }
    batcher.submit(req(5, "cancelled", 10_000), 0).unwrap();
    batcher.cancel(5).unwrap();
    batcher.submit(req(6, "expired", 0), 0).unwrap();

    let out = batcher.flush(0);
    // The expired request was answered at submit time, so flush returns the
    // four live requests plus the cancelled one.
    assert_eq!(out.len(), 5);
    assert_eq!(
        out.iter()
            .filter(|(_, o)| matches!(o, BatchOutcome::Done { .. }))
            .count(),
        4
    );
    assert!(out
        .iter()
        .any(|(_, o)| matches!(o, BatchOutcome::Cancelled)));
    // The already-expired request never ran and is answerable at submit.
    assert!(matches!(
        batcher.outcome(6),
        Some(BatchOutcome::DeadlineExceeded)
    ));

    // Throughput: four requests became exactly one engine invocation.
    assert_eq!(backend.invocations(), 1);
}
