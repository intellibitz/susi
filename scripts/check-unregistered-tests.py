#!/usr/bin/env python3
"""Every test file under a crate's src/ must be declared as a module.

A `.rs` file under `crates/*/src/tests/` that no `mod` declares is never compiled:
it looks like coverage and provides none. Three instances were found by hand - two
mastery tests that had never run and a preserved change that was nothing but a
registration - so this checks it mechanically, and CI runs it so a new dead test
cannot land.

Integration tests under a crate's top-level `tests/` directory are discovered by
cargo and need no declaration; this only looks inside `src/`.

    scripts/check-unregistered-tests.py
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def declares(text, stem, filename):
    """Either declaration form this codebase uses."""
    if re.search(rf"^\s*(?:pub\s+)?mod\s+{re.escape(stem)}\s*;", text, re.M):
        return True
    return re.search(rf'#\[path\s*=\s*"[^"]*{re.escape(filename)}"\]', text) is not None


def main():
    dead = []
    for test_file in sorted(ROOT.glob("crates/*/src/**/tests/*.rs")):
        if test_file.name == "mod.rs":
            continue
        crate_src = test_file.parents[1]
        for source in sorted(crate_src.rglob("*.rs")):
            if source == test_file:
                continue
            try:
                text = source.read_text(errors="ignore")
            except OSError:
                continue
            if declares(text, test_file.stem, test_file.name):
                break
        else:
            dead.append(test_file.relative_to(ROOT))
    if dead:
        print(f"❌ {len(dead)} test file(s) no module declares — they never compile:")
        for path in dead:
            print(f"     {path}")
        print("   declare each one in its crate (cargo fmt puts it in order):")
        print('     #[cfg(test)]')
        print(f'     #[path = "tests/<file>.rs"]')
        print("     mod <stem>_tests;")
        return 1
    total = len(list(ROOT.glob("crates/*/src/**/tests/*.rs")))
    print(f"✅ every test file under a crate's src is declared ({total} checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
