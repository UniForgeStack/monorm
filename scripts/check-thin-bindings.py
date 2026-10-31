#!/usr/bin/env python3
"""Fail when a language binding holds the text of a SQL statement.

Usage: python3 scripts/check-thin-bindings.py [ROOT]
The Rust core plans and builds all SQL. A binding converts types, maps errors and drives the async runtime.
The check reads the string literals of the code in the binding directories and skips comments and docstrings.
A kept site goes in `ALLOWED` with the reason. An entry that matches no literal fails the check, so the list cannot go stale.
"""

import ast
import re
import sys
from dataclasses import dataclass
from pathlib import Path

SCANNED = [
    ("languages/python/native/src", ".rs"),
    ("languages/js/native/src", ".rs"),
    ("languages/python/monorm", ".py"),
    ("languages/js/src", ".ts"),
]


@dataclass(frozen=True)
class Allowed:
    path: str
    snippet: str  # a part of the literal
    reason: str


ALLOWED: list[Allowed] = []

# The keywords are upper case: error text uses the lower case words ("select exactly one profile").
STATEMENT = re.compile(
    r"""(?x)
    (?:^|[\s;(])
    (?: SELECT\s+(?:[^\s]+\s+)*?FROM\b
      | SELECT\s+(?:DISTINCT\s+|\*|COUNT\s*\(|EXISTS\s*\(|[a-z_]+\s*\()
      | INSERT\s+INTO\b
      | UPDATE\s+\S+\s+SET\b
      | DELETE\s+FROM\b
      | COPY\s+\S+\s+(?:FROM|TO)\b
      | (?:RELEASE\s+)?SAVEPOINT\s+\S
      | ROLLBACK(?:\s+TO\b|\s+TRANSACTION\b|\s*;|$)
      | BEGIN(?:\s+(?:TRANSACTION|IMMEDIATE|DEFERRED|EXCLUSIVE|ISOLATION)\b|\s*;|$)
      | COMMIT(?:\s+TRANSACTION\b|\s*;|$)
      | LOCK\s+TABLE\b
      | SET\s+(?:LOCAL|SESSION|TRANSACTION)\s
      | (?:CREATE|ALTER|DROP)\s+(?:UNIQUE\s+)?(?:TABLE|INDEX|VIEW|SCHEMA|TYPE|FUNCTION|TRIGGER|EXTENSION|SEQUENCE)\b
      | TRUNCATE\s+(?:TABLE\s+)?\S
      | PRAGMA\s+\w
      | EXPLAIN\s+(?:ANALYZE\b|\()
      | WITH\s+(?:RECURSIVE\s+)?\S+\s+AS\s*\(
    )
    """
)


def is_statement(text: str) -> bool:
    return STATEMENT.search(text) is not None


def python_literals(source: str) -> list[tuple[int, str]]:
    """String constants of code. A docstring is a bare string statement and is skipped."""
    tree = ast.parse(source)
    docs = {
        id(node.value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str)
    }
    return [
        (node.lineno, node.value)
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docs
    ]


def scan_literals(source: str, rust: bool) -> list[tuple[int, str]]:
    """String literals of Rust or TypeScript code, without comments (a small scanner, not a parser)."""
    out: list[tuple[int, str]] = []
    i, n, line = 0, len(source), 1
    while i < n:
        c = source[i]
        if c == "\n":
            line += 1
        elif source.startswith("//", i):
            while i < n and source[i] != "\n":
                i += 1
            continue
        elif source.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                    continue
                if rust and source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                    continue
                line += source[i] == "\n"
                i += 1
            continue
        elif rust and c == "r" and (m := re.compile(r'r(#*)"').match(source, i)) and not (i and source[i - 1].isalnum()):
            end = source.find('"' + m[1], m.end())
            end = n if end < 0 else end
            out.append((line, source[m.end() : end]))
            line += source.count("\n", i, end)
            i = end + 1 + len(m[1])
            continue
        elif rust and c == "'" and (m := re.compile(r"'(?:\\.[^']*|[^\\'])'").match(source, i)):
            i = m.end()  # a char literal such as '"'; a lifetime has no closing quote
            continue
        elif c in "\"'`" and (c != "'" or not rust):
            start, start_line, i = i + 1, line, i + 1
            while i < n and source[i] != c:
                if source[i] == "\\":
                    i += 1
                line += source[i] == "\n"
                i += 1
            out.append((start_line, source[start:i]))
            i += 1
            continue
        i += 1
    return out


def literals(path: Path) -> list[tuple[int, str]]:
    source = path.read_text(encoding="utf-8")
    return python_literals(source) if path.suffix == ".py" else scan_literals(source, path.suffix == ".rs")


def scan(root: Path, allowed: list[Allowed] = ALLOWED) -> list[str]:
    problems: list[str] = []
    used: set[Allowed] = set()
    for directory, suffix in SCANNED:
        for path in sorted((root / directory).rglob(f"*{suffix}")):
            rel = path.relative_to(root).as_posix()
            for line, text in literals(path):
                if not is_statement(text):
                    continue
                kept = next((a for a in allowed if a.path == rel and a.snippet in text), None)
                if kept:
                    used.add(kept)
                else:
                    problems.append(f"{rel}:{line}: SQL text in a binding: {' '.join(text.split())[:80]!r}")
    problems += [f"stale allow list entry: {a.path}: {a.snippet!r}" for a in allowed if a not in used]
    return problems


def main() -> int:
    root = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
    problems = scan(root)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print("Move the SQL into monorm-core or monorm-engine, or add the site to ALLOWED with a reason.", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
