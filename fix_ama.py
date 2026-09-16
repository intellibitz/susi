import re

with open('src/gawd/ama.rs', 'r') as f:
    code = f.read()

# Modify solve_clean
old_solve_clean = """    pub fn solve_clean(&self, goal: &str, workspace: &Path, version: &str) -> String {
        self.solve_stream(goal, workspace, version)
    }"""
new_solve_clean = """    pub fn solve_clean(&self, goal: &str, workspace: &Path, version: &str) -> String {
        self.solve_stream(goal, workspace, version, &|piece| {
            use std::io::Write;
            print!("{}", piece);
            let _ = std::io::stdout().flush();
        })
    }"""
code = code.replace(old_solve_clean, new_solve_clean)

# Modify solve_stream signature
old_solve_stream = """    pub fn solve_stream(&self, goal: &str, workspace: &Path, version: &str) -> String {"""
new_solve_stream = """    pub fn solve_stream(&self, goal: &str, workspace: &Path, version: &str, callback: &dyn Fn(String)) -> String {"""
code = code.replace(old_solve_stream, new_solve_stream)

# Replace the inner generate_reasoning_stream call
old_inner_stream = """        } else {
            crate::gemi::engine::GemiEngine::generate_reasoning_stream(
                &reasoning_prompt,
                workspace,
                &|token| {
                    print!("{}", token);
                    let _ = std::io::stdout().flush();
                },
            )
        };"""
new_inner_stream = """        } else {
            crate::gemi::engine::GemiEngine::generate_reasoning_stream(
                &reasoning_prompt,
                workspace,
                callback,
            )
        };"""
code = code.replace(old_inner_stream, new_inner_stream)

with open('src/gawd/ama.rs', 'w') as f:
    f.write(code)

# Fix main.rs
with open('src/main.rs', 'r') as f:
    main_code = f.read()
main_code = main_code.replace("ama.solve_stream(&pulse.intent, &w, SUSI_VERSION)", "ama.solve_stream(&pulse.intent, &w, SUSI_VERSION, &|_| {})")
main_code = main_code.replace('ama.solve_stream("identity", &cwd, SUSI_VERSION)', 'ama.solve_stream("identity", &cwd, SUSI_VERSION, &|_| {})')
main_code = main_code.replace('ama.solve_stream(&goal, &cwd, SUSI_VERSION)', 'ama.solve_stream(&goal, &cwd, SUSI_VERSION, &|_| {})')
main_code = main_code.replace('ama.solve_stream(&input, &cwd, SUSI_VERSION)', 'ama.solve_stream(&input, &cwd, SUSI_VERSION, &|_| {})')
with open('src/main.rs', 'w') as f:
    f.write(main_code)

# Fix daemon/server.rs
with open('src/daemon/server.rs', 'r') as f:
    daemon_code = f.read()
daemon_code = daemon_code.replace("ama.solve_stream(&pulse.intent, &workspace_pulse, crate::SUSI_VERSION)", "ama.solve_stream(&pulse.intent, &workspace_pulse, crate::SUSI_VERSION, &|_| {})")
with open('src/daemon/server.rs', 'w') as f:
    f.write(daemon_code)

