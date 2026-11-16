//! Tools owned by one local session, never registered or serialized globally.
//!
//! The current-thread binding is only a convenience for existing call sites.
//! Workers must explicitly capture [`current`] and enter it with [`with_scope`].
//! The owner retains configuration and process handles behind the trait object.

use crate::progress::Cancel;
use serde_json::Value;
use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// One advertised tool and the extra authority its transport requires.
#[derive(Clone)]
pub struct ScopedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub required_actions: Vec<(String, String)>,
}

/// One owned tool attempt. The dispatcher reports its typed final result
/// after authority, execution and cancellation checks. Implementations may
/// use `Drop` to report an interrupted attempt when dispatch unwinds.
pub trait ScopedToolObserver: Send {
    fn finished(&mut self, outcome: &Result<String, String>);
}

/// An owned tool connection. Implementations keep credentials out of catalog rows.
pub trait ScopedToolProvider: Send + Sync {
    fn tools(&self) -> &[ScopedTool];
    /// Optional observation only; this grants no authority to execute.
    fn observe_call(&self, _name: &str) -> Option<Box<dyn ScopedToolObserver>> {
        None
    }
    /// Refuse a permanently closed owner or otherwise unavailable connection.
    fn check(&self) -> Result<(), String>;
    /// Called only after the normal tool authority and evidence checks.
    /// Blocking I/O must observe both cancellation tokens and owner closure.
    /// `cancel` may belong to a narrower DAG child; `turn_cancel` always
    /// belongs to the original session turn and cannot be masked by it.
    #[allow(clippy::too_many_arguments)] // explicit authority and both cancellation owners
    fn call(
        &self,
        name: &str,
        args: &Value,
        workspace: &Path,
        mission: Option<&str>,
        cancel: Option<Cancel>,
        turn_cancel: Cancel,
    ) -> Result<String, String>;
}

/// One turn's view of session-owned tools. Clones share the same cancellation.
pub struct ToolScope {
    provider: Arc<dyn ScopedToolProvider>,
    cancel: Cancel,
}

impl ToolScope {
    pub fn new(provider: Arc<dyn ScopedToolProvider>, cancel: Cancel) -> Arc<Self> {
        Arc::new(Self { provider, cancel })
    }

    pub fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Acquire) {
            return Err("session tool turn was cancelled".into());
        }
        self.provider.check()
    }

    pub fn tools(&self) -> &[ScopedTool] {
        self.provider.tools()
    }

    pub fn resolve(&self, name: &str) -> Option<&ScopedTool> {
        self.tools().iter().find(|tool| tool.name == name)
    }

    /// Unknown and legacy names never create an owned-tool observer.
    pub fn observe_call(&self, name: &str) -> Option<Box<dyn ScopedToolObserver>> {
        self.resolve(name)?;
        self.provider.observe_call(name)
    }

    /// Pass the original turn token explicitly, even when a DAG child has
    /// installed a narrower cancellation token on the calling thread.
    pub fn call(
        &self,
        name: &str,
        args: &Value,
        workspace: &Path,
        mission: Option<&str>,
    ) -> Result<String, String> {
        self.check()?;
        if self.resolve(name).is_none() {
            return Err(format!("unknown session tool '{name}'"));
        }
        let cancel = crate::progress::current_cancel().unwrap_or_else(|| self.cancel.clone());
        if cancel.load(Ordering::Acquire) {
            return Err("session tool operation was cancelled".into());
        }
        let result = self.provider.call(
            name,
            args,
            workspace,
            mission,
            Some(cancel.clone()),
            self.cancel.clone(),
        );
        self.check()?;
        if cancel.load(Ordering::Acquire) {
            return Err("session tool operation was cancelled".into());
        }
        result
    }
}

thread_local! {
    static SCOPE: RefCell<Option<Arc<ToolScope>>> = const { RefCell::new(None) };
}

pub fn current() -> Option<Arc<ToolScope>> {
    SCOPE.with(|scope| scope.borrow().clone())
}

/// Restore the prior binding on normal return and unwinding alike.
pub fn with_scope<T>(scope: Option<Arc<ToolScope>>, call: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<ToolScope>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPE.with(|scope| *scope.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SCOPE.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), scope)));
    call()
}

#[cfg(test)]
#[path = "tests/scoped_tools.rs"]
mod tests;
