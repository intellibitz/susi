import re

with open('src/gawd/task_manager.rs', 'r') as f:
    code = f.read()

code = code.replace('let idle = now_secs.saturating_sub(last_prog);', 'let _idle = now_secs.saturating_sub(last_prog);')
code = code.replace('let thresh = (record.expected_idle_ms / 1000).max(1);', 'let _thresh = (record.expected_idle_ms / 1000).max(1);')

with open('src/gawd/task_manager.rs', 'w') as f:
    f.write(code)

