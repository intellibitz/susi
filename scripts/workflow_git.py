"""Use the active Git install's native executable and shell on Windows."""
import os
from pathlib import Path
import shutil
import subprocess


def git_executable():
    binary = shutil.which("git") or "git"
    if os.name != "nt":
        return binary, None
    exe = Path(binary)
    candidates = [exe.parent.parent, exe.parent.parent.parent]
    root = next((p for p in candidates if (p / "usr/bin/bash.exe").is_file()), None)
    if root is None:
        output = subprocess.check_output([binary, "--exec-path"])
        root = Path(os.fsdecode(output).strip()).parents[2]
    for arch in ("ucrt64", "mingw64", "clang64", "clangarm64", "mingw32"):
        native = root / arch / "bin/git.exe"
        if native.is_file():
            binary = str(native)
            break
    return binary, root / "usr/bin/bash.exe"
