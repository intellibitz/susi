#!/usr/bin/env python3
"""Decide whether production code reaches a symbol.

Verdicts were prose, and prose lied in both directions. Thirteen roadmap vectors
claimed DELIVERED for components that exist and are tested but that no production
path reaches (`reserve_dispatch`, documented as reserving "right before
dispatch", called from nowhere). Then, going faster, I produced three wrong
refutations in one hour: 77 "orphaned artifacts" that were really test names and
module declarations, a nextest filter artifact read as "nothing executes it", and
prose read as tests.

Every one of those mistakes counted the wrong thing. This counts one thing: call
sites in non-test code. Comments, string literals, module declarations and
`#[cfg(test)]` blocks are stripped before counting, and a non-distinctive name is
refused outright - `load` matched 264 sites and proved nothing.

    scripts/check-reachability.py --delivered Symbol   # 0 if a production path reaches it
    scripts/check-reachability.py --refuted Symbol     # 0 if nothing in production reaches it
    scripts/check-reachability.py --report Symbol      # the call sites, for reading
    scripts/check-reachability.py --audit-verdicts     # every delivered verdict vs reachability
    scripts/check-reachability.py --self-test
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Names that match hundreds of sites and therefore prove nothing. Refusing them
# is the whole lesson of the hour spent trusting greps.
STOPWORDS = {
    "load", "record", "rank", "new", "run", "get", "set", "build", "main", "from",
    "into", "test", "index", "value", "len", "name", "call", "create", "open",
    "write", "read", "start", "stop", "send", "recv", "spawn", "parse", "format",
}
MIN_LEN = 6

DECLARATION = re.compile(r"^\s*(?:pub\s+)?(?:use|mod)\b")


def _blank_strings(text: str) -> str:
    """Replace string literals with empty ones, keeping line structure."""
    out = []
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch == "r" and i + 1 < n and text[i + 1] in '#"':
            j = i + 1
            while j < n and text[j] == "#":
                j += 1
            if j < n and text[j] == '"':
                hashes = j - (i + 1)
                closer = '"' + "#" * hashes
                end = text.find(closer, j + 1)
                end = n if end == -1 else end + len(closer)
                out.append(" " * (end - i))
                i = end
                continue
        if ch == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    break
                j += 1
            out.append(" " * (min(j, n - 1) + 1 - i))
            i = min(j, n - 1) + 1
            continue
        out.append(ch)
        i += 1
    return "".join(out)


def _blank_comments(text: str) -> str:
    """Replace comments with spaces, keeping newlines."""
    out = []
    i, n = 0, len(text)
    depth = 0
    while i < n:
        if text.startswith("/*", i):
            depth += 1
            out.append("  ")
            i += 2
            continue
        if depth and text.startswith("*/", i):
            depth -= 1
            out.append("  ")
            i += 2
            continue
        if depth:
            out.append("\n" if text[i] == "\n" else " ")
            i += 1
            continue
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j == -1 else j
            out.append(" " * (j - i))
            i = j
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def _strip_cfg_test(text: str) -> str:
    """Remove every #[cfg(...test...)] module, by brace matching."""
    pattern = re.compile(r"#\[cfg\([^)]*\btest\b[^)]*\)\]")
    while True:
        match = pattern.search(text)
        if not match:
            return text
        brace = text.find("{", match.end())
        if brace == -1:
            text = text[: match.start()]
            continue
        depth = 0
        i = brace
        while i < len(text):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        text = text[: match.start()] + "\n" * text[match.start(): i + 1].count("\n") + text[i + 1:]
        if i >= len(text):
            return text


def production_text(path: Path) -> str:
    """This file's code with comments, strings and test modules removed."""
    try:
        raw = path.read_text(errors="ignore")
    except OSError:
        return ""
    return _strip_cfg_test(_blank_comments(_blank_strings(raw)))


def is_distinctive(symbol: str) -> str | None:
    if len(symbol) < MIN_LEN:
        return f"{symbol!r} is shorter than {MIN_LEN} characters"
    if symbol.lower() in STOPWORDS:
        return f"{symbol!r} is a common word: it matches sites that prove nothing"
    return None


def source_files(root: Path):
    for base in ("crates", "src"):
        start = root / base
        if not start.is_dir():
            continue
        for path in start.rglob("*.rs"):
            if "target" in path.parts:
                continue
            if any(part in {"tests", "benches", "examples"} for part in path.parts):
                continue
            yield path


def call_sites(symbol: str, root: Path, as_function: bool = False):
    """Production call sites, with the defining file reported separately."""
    if as_function:
        pattern = re.compile(rf"\b{re.escape(symbol)}\s*\(")
    else:
        pattern = re.compile(rf"\b{re.escape(symbol)}\b")
    defining, sites = [], []
    for path in source_files(root):
        text = production_text(path)
        for number, line in enumerate(text.splitlines(), 1):
            if not pattern.search(line):
                continue
            # Only a declaration of THIS symbol: a call on a line that also defines
            # something else (`fn go() { symbol(2) }`) is a call site, not a definition.
            defines = re.search(
                rf"\b(?:fn|struct|enum|trait|type|const|static|impl)\s+{re.escape(symbol)}\b", line
            )
            if defines or DECLARATION.search(line):
                defining.append((path, number, line.strip()))
                continue
            sites.append((path, number, line.strip()))
    return defining, sites


def report(symbol: str, root: Path, as_function: bool) -> int:
    defining, sites = call_sites(symbol, root, as_function)
    if not defining and not sites:
        print(f"❌ {symbol}: no occurrence in production code at all")
        return 1
    print(f"  {symbol}: {len(sites)} production call site(s) in {len({s[0] for s in sites})} file(s)")
    for path, number, line in sites[:10]:
        print(f"    {path.relative_to(root)}:{number}: {line[:96]}")
    for path, number, line in defining[:3]:
        print(f"    (declared) {path.relative_to(root)}:{number}: {line[:88]}")
    return 0


def audit_verdicts(root: Path) -> int:
    """Every delivered verdict: does production reach anything it names?"""
    verdicts = sorted((root / ".agents/roadmap-verdicts").glob("*.json"))
    reachable = unreachable = unnamed = 0
    for path in verdicts:
        try:
            record = json.loads(path.read_text())
        except ValueError:
            continue
        if record.get("verdict") != "delivered":
            continue
        evidence = str(record.get("evidence") or "")
        candidates = [w for w in re.findall(r"[A-Za-z_][A-Za-z0-9_]{7,}", evidence) if not is_distinctive(w)]
        chosen = None
        for symbol in candidates:
            defining, sites = call_sites(symbol, root)
            if defining or sites:
                chosen = (symbol, sites)
                break
        if not chosen and candidates:
            unnamed += 1
            print(f"  ⚠️  {record.get('vector')}: its evidence names only test-only symbols "
                  f"({candidates[0]}) — nothing in production code matches them")
        elif not chosen:
            unnamed += 1
            print(f"  – {record.get('vector')}: no distinctive symbol in its evidence to check")
        elif chosen[1]:
            reachable += 1
            print(f"  ✅ {record.get('vector')}: {chosen[0]} reached from production ({len(chosen[1])} site(s))")
        else:
            unreachable += 1
            print(f"  ❌ {record.get('vector')}: {chosen[0]} is not reached from production code — the claim needs re-checking")
    print(f"  audit: {reachable} reachable, {unreachable} not reachable, {unnamed} without a distinctive symbol")
    return 0


def self_test() -> int:
    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "crates" / "demo" / "src").mkdir(parents=True)
        (root / "crates" / "demo" / "src" / "lib.rs").write_text(
            "pub fn distinctive_alpha_call(x: u32) -> u32 { x }\n"
            "pub fn orphaned_beta_call(x: u32) -> u32 { x }\n"
            "// a comment claiming orphaned_beta_call is used elsewhere\n"
            "/// doc mentioning distinctive_alpha_call in prose\n"
            "pub const NOTE: &str = \"orphaned_beta_call\";\n"
            "#[cfg(test)]\nmod tests {\n"
            "    #[test]\n    fn t() { orphaned_beta_call(1); }\n}\n"
        )
        (root / "crates" / "demo" / "src" / "user.rs").write_text(
            "use crate::distinctive_alpha_call;\n"
            "pub fn go() -> u32 { distinctive_alpha_call(2) }\n"
        )
        cases.append(("delivered finds a real call site", call_sites("distinctive_alpha_call", root)[1] != []))
        cases.append(("refuted ignores comments, strings and cfg(test)", call_sites("orphaned_beta_call", root)[1] == []))
        cases.append(("a use declaration is not a call site", len(call_sites("distinctive_alpha_call", root)[1]) == 1))
        cases.append(("common words are refused", is_distinctive("load") is not None))
        cases.append(("short names are refused", is_distinctive("run") is not None))
        cases.append(("distinctive names are accepted", is_distinctive("reserve_dispatch") is None))
    failed = 0
    for name, ok in cases:
        print(f"  {'✅' if ok else '❌'} {name}")
        failed += 0 if ok else 1
    print(f"  self-test: {len(cases) - failed}/{len(cases)} cases hold")
    return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--delivered", metavar="SYMBOL")
    parser.add_argument("--refuted", metavar="SYMBOL")
    parser.add_argument("--report", metavar="SYMBOL")
    parser.add_argument("--function", action="store_true", help="count only SYMBOL( call sites")
    parser.add_argument("--audit-verdicts", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=str(ROOT))
    args = parser.parse_args()
    root = Path(args.root).resolve()

    if args.self_test:
        return self_test()
    if args.audit_verdicts:
        return audit_verdicts(root)
    for flag, symbol in (("--delivered", args.delivered), ("--refuted", args.refuted), ("--report", args.report)):
        if not symbol:
            continue
        wrong = is_distinctive(symbol)
        if wrong:
            print(f"❌ {wrong}. Pick a name that occurs only where this capability does.")
            return 2
        if flag == "--report":
            return report(symbol, root, args.function)
        defining, sites = call_sites(symbol, root, args.function)
        if flag == "--delivered":
            if sites:
                print(f"✅ {symbol} is reached from production: {len(sites)} call site(s), first {sites[0][0].relative_to(root)}:{sites[0][1]}")
                return 0
            where = f"defined in {defining[0][0].relative_to(root)}:{defining[0][1]}" if defining else "not declared anywhere"
            print(f"❌ {symbol} is {where} and no production code calls it")
            return 1
        if sites:
            print(f"❌ {symbol} is reached from production ({len(sites)} call site(s)), so 'nothing reaches it' is false:")
            for path, number, line in sites[:5]:
                print(f"     {path.relative_to(root)}:{number}: {line[:88]}")
            return 1
        print(f"✅ {symbol} has no production call site ({len(defining)} declaration/reference site(s) only)")
        return 0
    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
