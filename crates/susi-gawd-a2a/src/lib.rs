#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! GAWD **A2A** tier: Agent2Agent wire protocol (`ra2a`).
//!
//! Depends on [`susi_gawd_agents`] only (`GawdAgentFleet`). Must not depend on
//! swarm or the host `susi-gawd` crate. `handler.rs` is kept on disk but left
//! out of the module tree (broken / unused).

pub mod capabilities;
pub mod executor;
pub mod task_store;

pub use capabilities::GawdCapabilities;
pub use executor::GawdA2AExecutor;
pub use task_store::GawdTaskStore;

#[cfg(test)]
mod tests {
    use super::*;
    use ra2a::server::{AgentExecutor, EventQueue, RequestContext};
    use ra2a::types::{Message, Part, TaskState};
    use std::sync::Arc;
    use susi_gawd_agents::GawdAgentFleet;

    /// Drives a future to completion on the bare test thread. The fleet's
    /// inference path internally creates a `tokio::runtime::Runtime` and calls
    /// `block_on`, which panics when invoked from inside any tokio runtime —
    /// so `#[tokio::test]` is unusable here.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        fn noop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        // Safe: the RawWaker's functions are no-ops over a null data pointer.
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = std::pin::pin!(fut);
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn orchestrator_capabilities_advertise_streaming_and_push() {
        let caps = GawdCapabilities::orchestrator();
        assert!(caps.streaming);
        assert!(caps.push_notifications);
        let wire = caps.as_capabilities();
        assert_eq!(wire.streaming, Some(true));
        assert_eq!(wire.push_notifications, Some(true));
    }

    #[test]
    fn default_capabilities_match_orchestrator() {
        let a = GawdCapabilities::default().as_capabilities();
        let b = GawdCapabilities::orchestrator().as_capabilities();
        assert_eq!(a.streaming, b.streaming);
        assert_eq!(a.push_notifications, b.push_notifications);
    }

    #[test]
    fn task_store_clones_share_inner_arc() {
        let store = GawdTaskStore::new();
        let clone = store.clone();
        assert!(Arc::ptr_eq(&store.inner(), &clone.inner()));
        let default = GawdTaskStore::default();
        assert!(!Arc::ptr_eq(&store.inner(), &default.inner()));
    }

    #[test]
    fn agent_card_advertises_orchestrator_identity() {
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let card = executor.agent_card();
        assert_eq!(card.name, "susi-gawd");
        assert!(!card.version.is_empty());
        assert_eq!(card.supported_interfaces.len(), 2);
        let bindings: Vec<String> = card
            .supported_interfaces
            .iter()
            .map(|i| i.protocol_binding.0.to_string())
            .collect();
        assert!(
            bindings.iter().any(|b| b.contains("JSONRPC")),
            "{bindings:?}"
        );
        assert!(bindings.iter().any(|b| b.contains("HTTP")), "{bindings:?}");
        assert!(card.skills.iter().any(|s| s.id == "agent_orchestration"));
        assert!(card.skills.iter().any(|s| s.id == "task_delegation"));
        assert!(card.capabilities.streaming.unwrap_or(false));
    }

    #[test]
    fn executor_task_store_is_shared() {
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let store = executor.task_store();
        assert!(Arc::ptr_eq(&store.inner(), &executor.task_store().inner()));
    }

    #[test]
    fn execute_emits_completed_task_with_agent_message() {
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let queue = EventQueue::new(8);
        let mut rx = queue.subscribe();
        let mut ctx = RequestContext::new("task-1", "ctx-1");
        ctx.message = Some(Message::user(vec![Part::text("status")]));

        block_on(executor.execute(&ctx, &queue)).expect("execute");

        let event = rx.try_recv().expect("task event");
        let ra2a::types::StreamResponse::Task(task) = event else {
            panic!("expected Task event");
        };
        assert_eq!(task.id.to_string(), "task-1");
        assert_eq!(task.context_id.to_string(), "ctx-1");
        assert_eq!(task.status.state, TaskState::Completed);
        let msg = task.status.message.expect("completed task carries message");
        assert!(!msg.parts.is_empty());
    }

    #[test]
    fn execute_without_message_still_completes() {
        // No message => empty content => fleet error maps to a Completed task
        // carrying the failure text, never a panic or silent drop.
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let queue = EventQueue::new(8);
        let mut rx = queue.subscribe();
        let ctx = RequestContext::new("task-2", "ctx-2");

        block_on(executor.execute(&ctx, &queue)).expect("execute");

        let ra2a::types::StreamResponse::Task(task) = rx.try_recv().expect("event") else {
            panic!("expected Task event");
        };
        assert_eq!(task.status.state, TaskState::Completed);
    }

    #[test]
    fn execute_with_non_text_part_completes_gracefully() {
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let queue = EventQueue::new(8);
        let mut rx = queue.subscribe();
        let mut ctx = RequestContext::new("task-3", "ctx-3");
        ctx.message = Some(Message::user(vec![]));

        block_on(executor.execute(&ctx, &queue)).expect("execute");
        let ra2a::types::StreamResponse::Task(task) = rx.try_recv().expect("event") else {
            panic!("expected Task event");
        };
        assert_eq!(task.status.state, TaskState::Completed);
    }

    #[test]
    fn cancel_emits_canceled_task() {
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let queue = EventQueue::new(8);
        let mut rx = queue.subscribe();
        let ctx = RequestContext::new("task-4", "ctx-4");

        block_on(executor.cancel(&ctx, &queue)).expect("cancel");

        let ra2a::types::StreamResponse::Task(task) = rx.try_recv().expect("event") else {
            panic!("expected Task event");
        };
        assert_eq!(task.id.to_string(), "task-4");
        assert_eq!(task.status.state, TaskState::Canceled);
        assert!(task.status.message.is_none());
    }
}
