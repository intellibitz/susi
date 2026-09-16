import re

with open('src/gawd/queue.rs', 'r') as f:
    code = f.read()

# Add thread notification for zero-latency wakeups without sleep loops
add_imports = """use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::Thread;"""
code = code.replace("""use std::path::{Path, PathBuf};
use std::sync::OnceLock;""", add_imports)

queue_struct = """pub struct SubstratePulseQueue {
    priority_queue: SegQueue<PulseEntry>,
    standard_queue: SegQueue<PulseEntry>,
}"""
new_queue_struct = """pub struct SubstratePulseQueue {
    priority_queue: SegQueue<PulseEntry>,
    standard_queue: SegQueue<PulseEntry>,
    consumer_thread: parking_lot::RwLock<Option<Thread>>,
}"""
code = code.replace(queue_struct, new_queue_struct)

new_fn = """    fn new() -> Self {
        Self {
            priority_queue: SegQueue::new(),
            standard_queue: SegQueue::new(),
        }
    }"""
new_fn_rep = """    fn new() -> Self {
        Self {
            priority_queue: SegQueue::new(),
            standard_queue: SegQueue::new(),
            consumer_thread: parking_lot::RwLock::new(None),
        }
    }"""
code = code.replace(new_fn, new_fn_rep)

ingest_end = """        } else {
            self.standard_queue.push(entry);
        }

        Ok(())"""
ingest_new = """        } else {
            self.standard_queue.push(entry);
        }

        if let Some(t) = self.consumer_thread.read().as_ref() {
            t.unpark();
        }

        Ok(())"""
code = code.replace(ingest_end, ingest_new)

register_consumer = """    pub fn clear(&self) {
        while self.priority_queue.pop().is_some() {}
        while self.standard_queue.pop().is_some() {}
    }
}"""
register_consumer_new = """    pub fn clear(&self) {
        while self.priority_queue.pop().is_some() {}
        while self.standard_queue.pop().is_some() {}
    }

    pub fn register_consumer(&self) {
        *self.consumer_thread.write() = Some(std::thread::current());
    }
}"""
code = code.replace(register_consumer, register_consumer_new)

with open('src/gawd/queue.rs', 'w') as f:
    f.write(code)

