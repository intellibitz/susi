import re

with open('src/gemi/engine.rs', 'r') as f:
    code = f.read()

# Replace the first spin lock
bad_lock_1 = """        let _guard = loop {
            if task_handle.is_cancelled() {
                return Err(EaiError::inference(
                    "Task cancelled while waiting for model load lock.",
                ));
            }
            match load_mutex.try_lock() {
                Ok(g) => break g,
                Err(_) => {
                    task_handle.report_progress(); // Keep waiting agents alive
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        };"""
good_lock_1 = """        let _guard = load_mutex.lock().unwrap();"""
code = code.replace(bad_lock_1, good_lock_1)

# Replace the second spin lock
bad_lock_2 = """        let wait_start = std::time::Instant::now();
        let mut substrate = loop {
            if task_handle.is_cancelled() {
                return Err(EaiError::inference(
                    "Task cancelled or stalled while waiting for model substrate.",
                ));
            }
            match substrate_shared.try_write() {
                Some(guard) => break guard,
                None => {
                    if wait_start.elapsed().as_millis() > 1000 {
                        return Err(EaiError::inference(
                            "Model substrate busy (Contention limit reached). Failing fast.",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        };"""
good_lock_2 = """        let mut substrate = substrate_shared.write();"""
code = code.replace(bad_lock_2, good_lock_2)

# Remove keep-alive hack
keep_alive_hack = """        // Blocking FFI Keep-Alive: Prevent watchdog timeouts during massive I/O model loads
        let is_loading = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let is_loading_clone = Arc::clone(&is_loading);
        let th_clone = Arc::clone(task_handle);
        std::thread::spawn(move || {
            while is_loading_clone.load(std::sync::atomic::Ordering::SeqCst) {
                th_clone.report_progress();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });

        let weights_result = llama::ModelWeights::from_gguf(model_data, &mut file, device);
        is_loading.store(false, std::sync::atomic::Ordering::SeqCst); // Stop keep-alive"""
good_keep_alive = """        let weights_result = llama::ModelWeights::from_gguf(model_data, &mut file, device);"""
code = code.replace(keep_alive_hack, good_keep_alive)

with open('src/gemi/engine.rs', 'w') as f:
    f.write(code)
