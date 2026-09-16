import re

with open('src/gawd/ama.rs', 'r') as f:
    code = f.read()

# Fix solve_with_streaming_trace signature
old_sig = """    fn solve_with_streaming_trace(
        &self,
        goal: &str,
        workspace: &Path,
        _version: &str,
    ) -> EaiResult<SusiMissionReport> {"""
new_sig = """    fn solve_with_streaming_trace(
        &self,
        goal: &str,
        workspace: &Path,
        _version: &str,
        callback: &dyn Fn(String),
    ) -> EaiResult<SusiMissionReport> {"""
code = code.replace(old_sig, new_sig)

# Fix solve_stream call to solve_with_streaming_trace
old_call = """        match self.solve_with_streaming_trace(goal, workspace, version) {"""
new_call = """        match self.solve_with_streaming_trace(goal, workspace, version, callback) {"""
code = code.replace(old_call, new_call)

with open('src/gawd/ama.rs', 'w') as f:
    f.write(code)

