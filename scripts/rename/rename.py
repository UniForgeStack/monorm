#!/usr/bin/env python3
"""Rename the project from `orm` to `monorm` with a reviewed rule list.

    python3 scripts/rename/rename.py              # dry-run (default): report only
    python3 scripts/rename/rename.py --apply      # git mv, then rewrite files
    python3 scripts/rename/rename.py --check      # guard: fail when an old name stays

`rules.toml` holds the protect rules, the rewrite rules, the env-var list and the
path moves. `allow.toml` holds the guard patterns and the allowed old-name sites.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path

HERE = Path(__file__).resolve().parent
SKIP_GLOBS = ("scripts/rename/**", "scripts/tests/rename/**", ".git/**", "bench/**/results/**")
PLACEHOLDER = "\x01{}\x02"
ENV_LOOKBEHIND = r"(?<![A-Za-z0-9_])"


@lru_cache(maxsize=None)
def glob_regex(glob: str) -> re.Pattern[str]:
    """Translate a glob: `**/` is any directories, `**` any text, `*` one segment."""
    out, i = [], 0
    while i < len(glob):
        if glob.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif glob.startswith("**", i):
            out.append(".*")
            i += 2
        elif glob[i] == "*":
            out.append("[^/]*")
            i += 1
        elif glob[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(glob[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


def matches_any(path: str, globs: list[str] | tuple[str, ...]) -> bool:
    return any(glob_regex(g).match(path) for g in globs)


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=root, check=True, capture_output=True, text=True).stdout


def tracked(root: Path) -> list[str]:
    return [p for p in git(root, "ls-files", "-z").split("\0") if p]


def is_scoped(path: str) -> bool:
    return not matches_any(path, SKIP_GLOBS)


def read_text(root: Path, path: str) -> str | None:
    """File text, or None for a missing, binary or non-UTF-8 file."""
    try:
        data = (root / path).read_bytes()
    except OSError:
        return None
    if b"\0" in data[:8192]:
        return None
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return None


@dataclass
class Rule:
    id: str
    files: list[str]
    pattern: re.Pattern[str]
    replace: str | None
    expect: tuple[int, int] | None
    protect: bool = False
    count: int = 0
    hit_files: set[str] = field(default_factory=set)


@dataclass
class Move:
    source: str
    sub: tuple[re.Pattern[str], str]


class RenameError(Exception):
    pass


def load_rules(path: Path) -> tuple[list[Rule], list[Move]]:
    data = tomllib.loads(path.read_text())
    rules: list[Rule] = []
    for raw in data.get("protect", []):
        rules.append(
            Rule(
                raw["id"],
                raw.get("files", ["**"]),
                re.compile(raw["pattern"]),
                None,
                None,
                protect=True,
            )
        )
    for raw in data.get("rule", []):
        expect = tuple(raw["expect"]) if "expect" in raw else None
        rules.append(
            Rule(
                raw["id"],
                raw.get("files", ["**"]),
                re.compile(raw["pattern"]),
                raw["replace"],
                expect,
            )
        )
    for raw in data.get("env_rename", []):
        name = raw["name"]
        rules.append(
            Rule(
                f"env-{name}",
                ["**"],
                re.compile(rf"{ENV_LOOKBEHIND}{re.escape(name)}\b"),
                "MON" + name,
                tuple(raw["expect"]),
            )
        )
    ids = [r.id for r in rules]
    dup = {i for i in ids if ids.count(i) > 1}
    if dup:
        raise RenameError(f"duplicate rule ids: {sorted(dup)}")
    moves = [Move(raw["from"], (re.compile(raw["sub"][0]), raw["sub"][1])) for raw in data.get("move", [])]
    return rules, moves


def plan_moves(root: Path, files: list[str], moves: list[Move]) -> list[tuple[str, str]]:
    """(old, new) pairs for every matched file or directory, deepest first."""
    dirs: set[str] = set()
    for f in files:
        parts = f.split("/")
        for n in range(1, len(parts)):
            dirs.add("/".join(parts[:n]))
    candidates = sorted(set(files) | dirs)
    found: dict[str, str] = {}
    for move in moves:
        regex = glob_regex(move.source)
        for path in candidates:
            if not regex.match(path):
                continue
            head, _, base = path.rpartition("/")
            new_base = move.sub[0].sub(move.sub[1], base)
            if new_base == base:
                continue
            new = f"{head}/{new_base}" if head else new_base
            if path in found and found[path] != new:
                raise RenameError(f"two moves for {path}: {found[path]} and {new}")
            found[path] = new
    planned = sorted(found.items(), key=lambda kv: (-kv[0].count("/"), kv[0]))
    # A target must not exist, and no two moves may share a target.
    existing = set(candidates)
    targets: set[str] = set()
    for old, new in planned:
        if (new in existing and new not in found) or new in targets or ((root / new).exists() and new not in found):
            raise RenameError(f"move target exists: {old} -> {new}")
        targets.add(new)
    return planned


def moved_path(path: str, planned: list[tuple[str, str]]) -> str:
    """Final path of a tracked file after all moves."""
    mapping = dict(planned)
    parts = path.split("/")
    out: list[str] = []
    for n in range(len(parts)):
        old = "/".join(parts[: n + 1])
        new = mapping.get(old)
        out.append(new.rsplit("/", 1)[-1] if new else parts[n])
    return "/".join(out)


def rewrite(text: str, path: str, rules: list[Rule]) -> str:
    """Apply the protect rules, then the rewrite rules, then restore the protected text."""
    saved: list[str] = []

    def mask(match: re.Match[str]) -> str:
        saved.append(match.group(0))
        return PLACEHOLDER.format(len(saved) - 1)

    for rule in rules:
        if not rule.protect or not matches_any(path, rule.files):
            continue
        text, n = rule.pattern.subn(mask, text)
        if n:
            rule.count += n
            rule.hit_files.add(path)
    for rule in rules:
        if rule.protect or not matches_any(path, rule.files):
            continue
        text, n = rule.pattern.subn(rule.replace, text)  # type: ignore[arg-type]
        if n:
            rule.count += n
            rule.hit_files.add(path)
    return re.sub(r"\x01(\d+)\x02", lambda m: saved[int(m.group(1))], text)


# --- guard -------------------------------------------------------------------


@dataclass
class Allow:
    glob: str
    pattern: re.Pattern[str]
    reason: str
    used: int = 0


@dataclass
class GuardPattern:
    id: str
    pattern: re.Pattern[str]
    files: list[str]
    exclude: list[str]
    path_only: bool


@dataclass
class Guard:
    patterns: list[GuardPattern]
    allows: list[Allow]


def load_guard(path: Path) -> Guard:
    data = tomllib.loads(path.read_text())
    patterns = [
        GuardPattern(
            g["id"],
            re.compile(g["pattern"]),
            g.get("files", ["**"]),
            g.get("exclude", []),
            g.get("target") == "path",
        )
        for g in data["guard"]
    ]
    allows = []
    for raw in data.get("allow", []):
        if not raw.get("reason", "").strip():
            raise RenameError(f"allow entry without a reason: {raw}")
        allows.append(Allow(raw["glob"], re.compile(raw["pattern"]), raw["reason"]))
    return Guard(patterns, allows)


def violates(guard: Guard, path: str, number: int, line: str) -> bool:
    """True when a guard pattern matches the line (line 0 is the path itself)."""
    return any(
        g.path_only == (number == 0)
        and matches_any(path, g.files)
        and not matches_any(path, g.exclude)
        and g.pattern.search(line)
        for g in guard.patterns
    )


def guard_hits(guard: Guard, path: str, text: str | None) -> list[tuple[str, int, str]]:
    """Old-name hits as (path, line number, line); line 0 is the path itself."""
    scanned = [(0, path)]
    if text is not None:
        scanned += list(enumerate(text.split("\n"), 1))
    return [(path, number, line) for number, line in scanned if violates(guard, path, number, line)]


def apply_allow(guard: Guard, hits: list[tuple[str, int, str]]) -> list[tuple[str, int, str]]:
    """Drop a hit only when the line is clean once the allowed text is cut out."""
    left = []
    for path, number, line in hits:
        rest = line
        for allow in guard.allows:
            if matches_any(path, [allow.glob]):
                rest, n = allow.pattern.subn("\x00", rest)
                allow.used += bool(n)
        if violates(guard, path, number, rest):
            left.append((path, number, line))
    return left


def scan(guard: Guard, items: list[tuple[str, str | None]]) -> tuple[list[tuple[str, int, str]], list[Allow]]:
    hits: list[tuple[str, int, str]] = []
    for path, text in items:
        hits += guard_hits(guard, path, text)
    left = apply_allow(guard, hits)
    stale = [a for a in guard.allows if a.used == 0]
    return left, stale


# --- driver ------------------------------------------------------------------


def run(root: Path, mode: str, rules_path: Path, allow_path: Path, out=print, verbose: bool = False) -> int:
    guard = load_guard(allow_path)
    files = [f for f in tracked(root) if is_scoped(f)]

    if mode == "check":
        items = [(f, read_text(root, f)) for f in files]
        left, stale = scan(guard, items)
        for path, number, line in left:
            out(f"{path}:{number}: {line.strip()[:160]}")
        for allow in stale:
            out(f"stale allow entry: {allow.glob} {allow.pattern.pattern!r} ({allow.reason})")
        return 1 if left or stale else 0

    rules, moves = load_rules(rules_path)
    planned = plan_moves(root, files, moves)
    new_texts: dict[str, str] = {}
    old_texts: dict[str, str] = {}
    for f in files:
        text = read_text(root, f)
        if text is None:
            continue
        old_texts[f] = text
        new_path = moved_path(f, planned)
        new = rewrite(text, new_path, rules)
        if new != text:
            new_texts[f] = new

    # A tree that has nothing left to rename is not an error, so a second --apply is a no-op.
    nothing_to_do = not planned and not new_texts
    problems = (
        []
        if nothing_to_do
        else [
            f"rule {r.id}: {r.count} matches, expected {r.expect[0]}..{r.expect[1]}"
            for r in rules
            if r.expect and not r.expect[0] <= r.count <= r.expect[1]
        ]
    )

    out(f"moves: {len(planned)}")
    for old, new in planned:
        out(f"  {old} -> {new}")
    out("rules:")
    for r in rules:
        want = f"{r.expect[0]}..{r.expect[1]}" if r.expect else "-"
        flag = "" if not r.expect or r.expect[0] <= r.count <= r.expect[1] else "  OUT OF RANGE"
        out(f"  {r.id}: {r.count} in {len(r.hit_files)} files (expect {want}){flag}")
        shown = sorted(r.hit_files) if verbose else sorted(r.hit_files)[:5]
        for path in shown:
            out(f"      {path}")
        if len(shown) < len(r.hit_files):
            out(f"      ... {len(r.hit_files) - len(shown)} more (--verbose lists all)")
    final = [(moved_path(f, planned), new_texts.get(f, old_texts.get(f))) for f in files]
    left, stale = scan(guard, final)
    out(f"uncovered hits: {len(left)}")
    for path, number, line in left:
        out(f"  {path}:{number}: {line.strip()[:160]}")
    for allow in stale:
        out(f"  stale allow entry: {allow.glob} {allow.pattern.pattern!r}")

    if problems:
        for p in problems:
            out(f"error: {p}")
        if mode == "apply":
            out("stopped before any write")
        return 1
    if mode == "apply":
        for old, new in planned:
            git(root, "mv", old, new)
        for f, text in new_texts.items():
            (root / moved_path(f, planned)).write_text(text)
        out(f"applied: {len(planned)} moves, {len(new_texts)} files rewritten")
        return 0
    return 1 if left or stale else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n")[0])
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--dry-run", action="store_true", help="report only (default)")
    group.add_argument("--apply", action="store_true", help="move and rewrite")
    group.add_argument("--check", action="store_true", help="fail when an old name stays")
    parser.add_argument("--verbose", action="store_true", help="list every file of each rule")
    parser.add_argument("--root", type=Path, default=HERE.parents[1])
    parser.add_argument("--rules", type=Path, default=HERE / "rules.toml")
    parser.add_argument("--allow", type=Path, default=HERE / "allow.toml")
    args = parser.parse_args(argv)
    mode = "apply" if args.apply else "check" if args.check else "dry-run"
    try:
        return run(args.root.resolve(), mode, args.rules, args.allow, verbose=args.verbose)
    except RenameError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
