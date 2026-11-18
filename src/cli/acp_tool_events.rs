//! One turn's bounded event handoff. Neither callbacks nor live senders escape it.
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use susi_core::scoped_tools::ScopedToolObserver;
use susi_gawd::acp::{
    AcpExecutorEvent, AcpToolCall, AcpToolCallContent, AcpToolCallStatus, AcpToolCallUpdate,
};

const POLL: Duration = Duration::from_millis(10);
const BUFFER: usize = 32;
const MAX_PENDING: usize = 64;
const MAX_TEXT: usize = 16 * 1024;

struct State {
    open: bool,
    pending: BTreeSet<String>,
}

pub(super) struct Events {
    sender: mpsc::SyncSender<AcpExecutorEvent>,
    state: Mutex<State>,
    receiving: AtomicBool,
}

fn preview(text: &str) -> String {
    let mut end = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = text[..end].to_owned();
    if end < text.len() {
        result.push_str("\n[truncated]");
    }
    result
}

fn terminal(id: String, status: AcpToolCallStatus, text: &str) -> AcpExecutorEvent {
    let mut update = AcpToolCallUpdate::new(id);
    update.status = Some(status);
    let redacted =
        susi_core::untrusted_content::wrap_tool_output("acp:tool-result", text).redacted_for_sink();
    update.content = Some(vec![AcpToolCallContent::text(preview(&redacted))]);
    AcpExecutorEvent::ToolCallUpdate(update)
}

impl Events {
    pub(super) fn new() -> (Arc<Self>, mpsc::Receiver<AcpExecutorEvent>) {
        let (sender, receiver) = mpsc::sync_channel(BUFFER);
        (
            Arc::new(Self {
                sender,
                state: Mutex::new(State {
                    open: true,
                    pending: BTreeSet::new(),
                }),
                receiving: AtomicBool::new(true),
            }),
            receiver,
        )
    }

    pub(super) fn observe(
        self: &Arc<Self>,
        id: String,
        name: &str,
        active: &AtomicBool,
        can_wait: impl Fn() -> bool,
    ) -> Option<Box<dyn ScopedToolObserver>> {
        let mut call = AcpToolCall::new(id.clone(), format!("Call {name}"));
        call.name = Some(name.to_owned());
        let mut event = AcpExecutorEvent::ToolCall(call);
        loop {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.open
                || !self.receiving.load(Ordering::Acquire)
                || !active.load(Ordering::Acquire)
            {
                return None;
            }
            if state.pending.len() < MAX_PENDING {
                match self.sender.try_send(event) {
                    Ok(()) => {
                        state.pending.insert(id.clone());
                        return Some(Box::new(Observer {
                            events: Arc::clone(self),
                            id,
                            finished: false,
                        }));
                    }
                    Err(mpsc::TrySendError::Full(returned)) => event = returned,
                    Err(mpsc::TrySendError::Disconnected(_)) => return None,
                }
            }
            // Observation must not delay a refused call behind a full queue.
            // The first nonblocking admission can still report that refusal.
            if !can_wait() {
                return None;
            }
            drop(state);
            std::thread::sleep(POLL);
        }
    }

    fn finish(&self, id: &str, outcome: &Result<String, String>) {
        let (status, text) = match outcome {
            Ok(text) => (AcpToolCallStatus::Completed, text),
            Err(text) => (AcpToolCallStatus::Failed, text),
        };
        let mut event = terminal(id.to_owned(), status, text);
        loop {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.open || !self.receiving.load(Ordering::Acquire) || !state.pending.contains(id)
            {
                return;
            }
            match self.sender.try_send(event) {
                Ok(()) => {
                    state.pending.remove(id);
                    return;
                }
                Err(mpsc::TrySendError::Full(returned)) => event = returned,
                Err(mpsc::TrySendError::Disconnected(_)) => return,
            }
            drop(state);
            std::thread::sleep(POLL);
        }
    }

    pub(super) fn message(&self, mut text: &str) {
        while !text.is_empty() {
            let mut end = text.len().min(MAX_TEXT);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let mut event = AcpExecutorEvent::MessageDelta(text[..end].to_owned());
            loop {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if !state.open || !self.receiving.load(Ordering::Acquire) {
                    return;
                }
                match self.sender.try_send(event) {
                    Ok(()) => break,
                    Err(mpsc::TrySendError::Full(returned)) => event = returned,
                    Err(mpsc::TrySendError::Disconnected(_)) => return,
                }
                drop(state);
                std::thread::sleep(POLL);
            }
            text = &text[end..];
        }
    }

    /// Freeze producers before the completion result is handed to the caller.
    /// Accepted starts always receive one terminal update, even when a mission
    /// worker returns while a cancelled tool worker is still unwinding.
    pub(super) fn close(&self, cancelled: bool) {
        let pending = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.open = false;
            std::mem::take(&mut state.pending)
        };
        let reason = if cancelled {
            "Tool call cancelled before completion"
        } else {
            "Turn ended before the tool call completed"
        };
        for id in pending {
            let mut event = terminal(id, AcpToolCallStatus::Failed, reason);
            while self.receiving.load(Ordering::Acquire) {
                match self.sender.try_send(event) {
                    Ok(()) => break,
                    Err(mpsc::TrySendError::Full(returned)) => event = returned,
                    Err(mpsc::TrySendError::Disconnected(_)) => return,
                }
                std::thread::sleep(POLL);
            }
        }
    }

    /// Used by the caller on normal completion and unwinding. A detached sender
    /// must not depend on receiver destruction or joining every clone.
    pub(super) fn stop_receiving(&self) {
        self.receiving.store(false, Ordering::Release);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.open = false;
        state.pending.clear();
    }
}

struct Observer {
    events: Arc<Events>,
    id: String,
    finished: bool,
}
impl ScopedToolObserver for Observer {
    fn finished(&mut self, outcome: &Result<String, String>) {
        self.finished = true;
        self.events.finish(&self.id, outcome);
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        if !self.finished {
            self.events.finish(
                &self.id,
                &Err("Tool execution ended without an outcome".into()),
            );
        }
    }
}

#[cfg(test)]
#[path = "tests/acp_tool_events.rs"]
mod tests;
