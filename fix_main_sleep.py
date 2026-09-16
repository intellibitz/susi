import re

with open('src/main.rs', 'r') as f:
    code = f.read()

bad_loop = """    std::thread::spawn(move || loop {
        if let Some(pulse) = queue.pop() {
            let _ = ama.solve_stream(&pulse.intent, &w, SUSI_VERSION);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    });"""

good_loop = """    std::thread::spawn(move || {
        queue.register_consumer();
        loop {
            if let Some(pulse) = queue.pop() {
                let _ = ama.solve_stream(&pulse.intent, &w, SUSI_VERSION);
            } else {
                std::thread::park(); // Zero-latency, zero-CPU waiting until a pulse is ingested
            }
        }
    });"""

code = code.replace(bad_loop, good_loop)

with open('src/main.rs', 'w') as f:
    f.write(code)

