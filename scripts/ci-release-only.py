#!/usr/bin/env python3
"""Recognize only a byte-exact SUSI release version bump, otherwise fail closed."""
import json
import os
import re
import subprocess
import sys
import tomllib

MANIFESTS = {
    "Cargo.toml": "susi",
    **{f"crates/susi-{name}/Cargo.toml": f"susi-{name}" for name in
       ("daemon", "gawd", "gemi", "gmcp", "harness", "server")},
}
METADATA = (".agents/evidence.json", ".agents/identity.json", ".agents/roadmap.json")
FILES = set(MANIFESTS) | set(METADATA) | {"Cargo.lock", "README.md"}
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")


def git(*args):
    return subprocess.check_output(["git", *args], stderr=subprocess.DEVNULL)


def blob(rev, path):
    return git("show", f"{rev}:{path}")


def is_release_only():
    base = git("merge-base", os.environ.get("BASE_REF", "origin/main"), "HEAD").decode().strip()
    changed = set(git("diff", "--no-renames", "--name-only", "-z", base, "HEAD").decode().split("\0")) - {""}
    if changed != FILES:
        return False
    before = {p: blob(base, p) for p in FILES}
    after = {p: blob("HEAD", p) for p in FILES}
    old = tomllib.loads(before["Cargo.toml"].decode())["package"]["version"]
    new = tomllib.loads(after["Cargo.toml"].decode())["package"]["version"]
    if not VERSION.fullmatch(old) or not VERSION.fullmatch(new):
        return False
    if tuple(map(int, new.split("."))) <= tuple(map(int, old.split("."))):
        return False
    old_bytes, new_bytes = old.encode(), new.encode()
    for path in FILES:
        expected_count = len(MANIFESTS) if path == "Cargo.lock" else 1
        if before[path].count(old_bytes) != expected_count:
            return False
        if before[path].replace(old_bytes, new_bytes) != after[path]:
            return False
    for path, name in MANIFESTS.items():
        for data, version in ((before[path], old), (after[path], new)):
            package = tomllib.loads(data.decode())["package"]
            if package["name"] != name or package["version"] != version:
                return False
    for path in METADATA:
        if json.loads(before[path])["version"] != old or json.loads(after[path])["version"] != new:
            return False
    for data, version in ((before["Cargo.lock"], old), (after["Cargo.lock"], new)):
        packages = tomllib.loads(data.decode())["package"]
        for name in MANIFESTS.values():
            matches = [p for p in packages if p["name"] == name and p["version"] == version and "source" not in p]
            if len(matches) != 1:
                return False
    return True


if __name__ == "__main__":
    try:
        sys.exit(0 if is_release_only() else 1)
    except (OSError, ValueError, KeyError, TypeError, UnicodeError, subprocess.SubprocessError):
        sys.exit(1)
