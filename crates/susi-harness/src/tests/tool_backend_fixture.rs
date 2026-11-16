//! Thread-local protocol replies for tests of core operator presentation.
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

struct Script {
    replies: VecDeque<Value>,
    calls: Vec<Value>,
}
thread_local! {
    static SCRIPT: RefCell<Option<Script>> = const { RefCell::new(None) };
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        SCRIPT.with_borrow_mut(|script| *script = None);
    }
}

pub(super) fn dispatch(
    name: &str,
    arguments: &Value,
    cwd: Option<&Path>,
) -> Option<EaiResult<(Value, Option<String>)>> {
    SCRIPT.with_borrow_mut(|slot| {
        let script = slot.as_mut()?;
        script.calls.push(json!({"name":name,"arguments":arguments,
            "cwd":cwd.map(susi_paths::native_path::encode).transpose().unwrap()}));
        Some(
            script
                .replies
                .pop_front()
                .map(|reply| (reply, Some("2025-11-25".into())))
                .ok_or_else(|| EaiError::protocol("unexpected extra fixture call")),
        )
    })
}

pub(crate) fn scripted<T>(replies: Vec<Value>, body: impl FnOnce() -> T) -> (T, Vec<Value>) {
    SCRIPT.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(Script {
            replies: replies.into(),
            calls: Vec::new(),
        });
    });
    let _reset = Reset;
    let result = body();
    let script = SCRIPT.with_borrow_mut(Option::take).unwrap();
    assert!(
        script.replies.is_empty(),
        "expected provider calls were not made"
    );
    (result, script.calls)
}
