import re

with open('src/gemi/engine.rs', 'r') as f:
    code = f.read()

code = code.replace("task_handle: &Arc<crate::gawd::task_manager::TaskHandle>", "_task_handle: &Arc<crate::gawd::task_manager::TaskHandle>")

with open('src/gemi/engine.rs', 'w') as f:
    f.write(code)
