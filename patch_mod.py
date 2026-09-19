import re

with open("src/gemi/mod.rs", "r") as f:
    content = f.read()

if "pub mod hf_discovery;" not in content:
    content = content.replace("pub mod hardware;", "pub mod hardware;\npub mod hf_discovery;")

with open("src/gemi/mod.rs", "w") as f:
    f.write(content)
