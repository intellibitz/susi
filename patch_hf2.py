import re
with open("src/gemi/hf_discovery.rs", "r") as f:
    text = f.read()

text = text.replace("step: step_idx,", "step: step_idx,\n                                fields: Default::default(),")
text = text.replace("step.step = (i + 1) as u32;", "step.step = i + 1;")

with open("src/gemi/hf_discovery.rs", "w") as f:
    f.write(text)
