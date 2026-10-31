#!/usr/bin/env bash
# Hash complete source and tool inputs in one native process. Git owns all
# index/ref interpretation; Python frames the data without shell subprocesses.
exec python3 - "$@" <<'PY'
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys

kind = sys.argv[1] if len(sys.argv) > 1 else "gate"
if kind not in ("gate", "--format"):
    sys.exit(2)
git_bin = shutil.which("git") or "git"
shell = None
if os.name == "nt":
    exe = Path(git_bin)
    candidates = [exe.parent.parent, exe.parent.parent.parent]
    root = next((p for p in candidates if (p / "usr/bin/bash.exe").is_file()), None)
    if root is None:
        output = subprocess.check_output([git_bin, "--exec-path"])
        root = Path(os.fsdecode(output).strip()).parents[2]
    shell = root / "usr/bin/bash.exe"
    for arch in ("ucrt64", "mingw64", "clang64", "clangarm64", "mingw32"):
        native = root / arch / "bin/git.exe"
        if native.is_file():
            git_bin = str(native)
            break

repo = Path(os.fsdecode(subprocess.check_output([git_bin, "rev-parse", "--show-toplevel"]).rstrip(b"\r\n")))
digest = hashlib.sha256()

def add(label, data):
    digest.update(label.encode() + b"\0")
    digest.update(len(data).to_bytes(8, "big"))
    digest.update(data)

def git(*args):
    return subprocess.check_output([git_bin, *args], cwd=repo)

def file_input(path):
    add("path", os.fsencode(path))
    content = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            content.update(chunk)
    add("content", content.digest())

def version(name, default):
    tool = os.environ.get(name) or default
    program = shutil.which(tool) or tool
    command = [program, "--version"]
    if shell and Path(program).suffix.lower() not in (".exe", ".cmd", ".bat"):
        command = [str(shell), "-c", 'exec "$1" --version', "proof-tool", tool]
    return subprocess.check_output(command, cwd=repo)

paths = [".", ":(exclude).agents/tasks"]
add("version", b"susi-proof-v3")
add("kind", kind.encode())
# A diff alone misses upstream changes outside this branch's edited paths.
# Only task bookkeeping is inert: other .agents data is compiled/tested.
add("index", git("ls-files", "--stage", "-z", "--", *paths))
add("edits", git("diff", "--binary", "--no-ext-diff", "--no-textconv", "--", *paths))
for raw in git("ls-files", "--others", "--exclude-standard", "-z", "--", *paths).split(b"\0"):
    if raw:
        file_input(repo / os.fsdecode(raw))
for name in sorted(os.environ):
    if name.startswith(("CARGO_", "RUST")):
        add(name, os.fsencode(os.environ[name]))
# Cargo reads configuration above the checkout as well as in CARGO_HOME.
configs = [p / ".cargo" / name for p in (repo, *repo.parents) for name in ("config", "config.toml")]
cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
configs.extend(cargo_home / name for name in ("config", "config.toml"))
for path in configs:
    if path.is_file():
        file_input(path)
add("formatter", version("RUSTFMT", "rustfmt"))
if kind != "--format":
    add("compiler", version("RUSTC", "rustc"))
    add("gate-policy", os.environ.get("SUSI_LOCAL_GATE", "").encode())
    add("scope", git("diff", "--binary", "--no-ext-diff", "--no-textconv", "origin/main", "--", *paths))
print(digest.hexdigest())
PY
