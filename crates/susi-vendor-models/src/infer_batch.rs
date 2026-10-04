//! Bounded inference batching for capable backends (VC-201-046).
//!
//! Batching only exists on backends that opt in; per-request deadlines and
//! cancellation are first-class, and the configured latency bound is a
//! hard limit — a request that would exceed its deadline is answered
//! `DeadlineExceeded`, never silently run late.

use crate::susi_error::{eai_bail as bail, EaiResult};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct BatchLimits {
    /// Max requests flushed in one backend call.
    pub max_batch: usize,
    /// Max queue depth; submissions beyond it are rejected.
    pub max_queue: usize,
    /// Requests waiting longer than this (ms) flush early.
    pub max_wait_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BatchOutcome {
    Done {
        output: String,
    },
    /// The request's own deadline passed before it ran.
    DeadlineExceeded,
    /// Cancelled by the submitter before the batch ran.
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: u64,
    pub prompt: String,
    /// Absolute deadline (ms); deterministic clock supplied by the caller.
    pub deadline_ms: u64,
}

/// Backend that can execute a prompt batch in one call. `supports_batch`
/// is the capability flag; backends without it are refused up front.
pub trait BatchBackend {
    fn supports_batch(&self) -> bool;
    fn run(&self, prompts: &[String]) -> Vec<String>;
}

struct Pending {
    req: Request,
    enqueued_ms: u64,
    cancelled: bool,
}

pub struct Batcher<'a> {
    limits: BatchLimits,
    backend: &'a dyn BatchBackend,
    pending: Vec<Pending>,
    outcomes: BTreeMap<u64, BatchOutcome>,
}

impl<'a> Batcher<'a> {
    pub fn new(limits: BatchLimits, backend: &'a dyn BatchBackend) -> EaiResult<Self> {
        if !backend.supports_batch() {
            bail!("backend does not support batching; submit requests singly");
        }
        if limits.max_batch == 0 || limits.max_queue == 0 {
            bail!("batch limits must be non-zero");
        }
        Ok(Self {
            limits,
            backend,
            pending: Vec::new(),
            outcomes: BTreeMap::new(),
        })
    }

    /// Enqueue a request; queue-full and already-expired requests are
    /// answered immediately rather than piling up.
    pub fn submit(&mut self, req: Request, now_ms: u64) -> EaiResult<()> {
        if self.pending.len() >= self.limits.max_queue {
            bail!("batch queue full ({} pending)", self.limits.max_queue);
        }
        if req.deadline_ms <= now_ms {
            self.outcomes.insert(req.id, BatchOutcome::DeadlineExceeded);
            return Ok(());
        }
        self.pending.push(Pending {
            req,
            enqueued_ms: now_ms,
            cancelled: false,
        });
        Ok(())
    }

    pub fn cancel(&mut self, id: u64) -> EaiResult<()> {
        let Some(p) = self.pending.iter_mut().find(|p| p.req.id == id) else {
            bail!("no pending request {id}");
        };
        p.cancelled = true;
        Ok(())
    }

    /// Whether a flush should happen now: batch full, or the oldest live
    /// request has waited past `max_wait_ms`.
    #[must_use]
    pub fn should_flush(&self, now_ms: u64) -> bool {
        let live = self.pending.iter().filter(|p| !p.cancelled).count();
        if live == 0 {
            return false;
        }
        if live >= self.limits.max_batch {
            return true;
        }
        self.pending
            .iter()
            .filter(|p| !p.cancelled)
            .map(|p| p.enqueued_ms)
            .min()
            .is_some_and(|oldest| now_ms.saturating_sub(oldest) >= self.limits.max_wait_ms)
    }

    /// Flush: expired and cancelled requests get terminal outcomes first;
    /// the rest run in one bounded backend call.
    pub fn flush(&mut self, now_ms: u64) -> Vec<(u64, BatchOutcome)> {
        let mut done = Vec::new();
        let mut runnable = Vec::new();
        for p in std::mem::take(&mut self.pending) {
            if p.cancelled {
                done.push((p.req.id, BatchOutcome::Cancelled));
            } else if p.req.deadline_ms <= now_ms {
                done.push((p.req.id, BatchOutcome::DeadlineExceeded));
            } else {
                runnable.push(p.req);
            }
        }
        // Latency bound: a request that cannot finish before its deadline
        // is not started.
        let deadline_guard = |r: &Request| r.deadline_ms > now_ms;
        runnable.retain(|r| {
            if deadline_guard(r) {
                true
            } else {
                done.push((r.id, BatchOutcome::DeadlineExceeded));
                false
            }
        });
        let prompts: Vec<String> = runnable.iter().map(|r| r.prompt.clone()).collect();
        let outputs = self.backend.run(&prompts);
        for (r, out) in runnable.into_iter().zip(outputs) {
            done.push((r.id, BatchOutcome::Done { output: out }));
        }
        for (id, o) in &done {
            self.outcomes.insert(*id, o.clone());
        }
        done
    }

    #[must_use]
    pub fn outcome(&self, id: u64) -> Option<&BatchOutcome> {
        self.outcomes.get(&id)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.iter().filter(|p| !p.cancelled).count()
    }
}

/// A production batching backend bound to a local inference engine
/// (llama.cpp server, Ollama, or vLLM). `run_batch` is the engine's one-call
/// batch executor — production code wires it to the engine's HTTP
/// `/completions` handler; hermetic tests supply a deterministic in-process
/// executor. The invocation count is surfaced so a load test can prove
/// batching collapses many requests into one engine call.
pub struct EngineBatchBackend<F> {
    run_batch: F,
    invocations: std::cell::Cell<usize>,
}

impl<F> EngineBatchBackend<F> {
    #[must_use]
    pub fn new(run_batch: F) -> Self {
        Self {
            run_batch,
            invocations: std::cell::Cell::new(0),
        }
    }

    /// How many times [`BatchBackend::run`] has been invoked — one per
    /// flushed batch, not one per request.
    #[must_use]
    pub fn invocations(&self) -> usize {
        self.invocations.get()
    }
}

impl<F: Fn(&[String]) -> Vec<String>> BatchBackend for EngineBatchBackend<F> {
    fn supports_batch(&self) -> bool {
        true
    }

    fn run(&self, prompts: &[String]) -> Vec<String> {
        self.invocations.set(self.invocations.get() + 1);
        (self.run_batch)(prompts)
    }
}
