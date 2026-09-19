import re

with open("src/sandbox/manager.rs", "r") as f:
    content = f.read()

old_ladder_fn = """    pub fn model_ladder(&self) -> Vec<ModelLadderConfigStep> {
        self.get_or_bundled_default("model_ladder")
    }"""

new_ladder_fn = """    pub fn model_ladder(&self) -> Vec<ModelLadderConfigStep> {
        let default_steps: Vec<ModelLadderConfigStep> = self.get_or_bundled_default("model_ladder");
        if default_steps.is_empty() {
            crate::gemi::hf_discovery::discover_dynamic_ladder()
        } else {
            default_steps
        }
    }"""

content = content.replace(old_ladder_fn, new_ladder_fn)

with open("src/sandbox/manager.rs", "w") as f:
    f.write(content)
