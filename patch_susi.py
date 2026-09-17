import os
import json

# 1. Update config.default.json
with open('config.default.json', 'r') as f:
    config = json.load(f)

if 'qdrant_url' not in config:
    config['qdrant_url'] = "http://localhost:6334"
if 'crates_io_api_url' not in config:
    config['crates_io_api_url'] = "https://crates.io/api/v1/crates"
if 'susi_alpha_default' not in config:
    config['susi_alpha_default'] = "susi-alpha"

with open('config.default.json', 'w') as f:
    json.dump(config, f, indent=2)


# 2. Patch src/gmcp/tools/mod.rs
tools_path = 'src/gmcp/tools/mod.rs'
with open(tools_path, 'r') as f:
    tools_code = f.read()

# Replace Qdrant hardcode with config resolution
if 'Qdrant::from_url("http://localhost:6334")' in tools_code:
    # First we need to make sure we can get config in mod.rs
    tools_code = tools_code.replace(
        'Qdrant::from_url("http://localhost:6334")',
        r'Qdrant::from_url(&crate::sandbox::SusiConfig::global().get_string("qdrant_url", "http://localhost:6334"))'
    )
    with open(tools_path, 'w') as f:
        f.write(tools_code)


# 3. Patch src/sandbox/manager.rs
manager_path = 'src/sandbox/manager.rs'
with open(manager_path, 'r') as f:
    manager_code = f.read()

# Replace default fallbacks with config gets
manager_code = manager_code.replace(
    'pub fn default_model(&self) -> String { self.get_string("default_model", "susi-alpha") }',
    'pub fn default_model(&self) -> String { self.get_string("default_model", &self.get_string("susi_alpha_default", "susi-alpha")) }' # still needs a string default if all else fails, because it returns String, not Option. Wait, we can't completely eliminate string fallbacks in rust unless we return Result or unwrap.
)

# Wait... If we remove hardcoded strings completely, we must raise errors if the config is missing.
