import json

with open("config/config.default.json", "r") as f:
    config = json.load(f)

# Clear it out so it triggers algorithmic discovery
config["model_ladder"] = []

with open("config/config.default.json", "w") as f:
    json.dump(config, f, indent=2)
