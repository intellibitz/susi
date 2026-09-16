import re

with open('src/gawd/ama.rs', 'r') as f:
    code = f.read()

# Fix `solve` call
old_solve = """        let res = self.solve_with_streaming_trace(goal, workspace, version);"""
new_solve = """        let res = self.solve_with_streaming_trace(goal, workspace, version, &|_| {});"""
code = code.replace(old_solve, new_solve)

with open('src/gawd/ama.rs', 'w') as f:
    f.write(code)

