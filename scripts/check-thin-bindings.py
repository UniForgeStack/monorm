#!/usr/bin/env python3
"""Fail when a language binding holds the text of a SQL statement, or checks the shape of an admin config.

Usage: python3 scripts/check-thin-bindings.py [ROOT]
The Rust core plans and builds all SQL. A binding converts types, maps errors and drives the async runtime.
The SQL rule reads the string literals of the code in the binding directories and skips comments and docstrings.
A kept site goes in `ALLOWED` with the reason. An entry that matches no literal fails the check, so the list cannot go stale.

The admin rule: the admin files of a binding make an `AdminConfigError` or an `AdminSettingsError` only in the error map
(`_admin_error`, `adminError`). Rust checks the shape of a config value and names the error code. A site that lowers a host object
(a function, a model class, a wrapper) sees what Rust cannot see: it goes in `ADMIN_ALLOWED` with the reason.
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

ADMIN_FILES = [
    ("languages/python/monorm", re.compile(r"(?:_admin_\w+|admin)\.py$")),
    ("languages/js/src", re.compile(r"admin(?:-\w+)?\.ts$")),
]
ERROR_MAPS = {"languages/python/monorm/admin.py": "_admin_error", "languages/js/src/admin.ts": "adminError"}
ADMIN_ALLOWED: list[Allowed] = [
    Allowed('languages/python/monorm/admin.py', 'a value of type', 'a literal of the admin: only the binding sees the Python type of the value'),
    Allowed('languages/python/monorm/admin.py', 'a badge value is a string', 'a badge key is a Python value: only the binding sees its type before it becomes text'),
    Allowed('languages/python/monorm/admin.py', 'files maps a field name', 'the `files` storage names stay in the binding, which owns the file storage registry'),
    Allowed('languages/python/monorm/admin.py', 'inlines is a list of', 'an inline wrapper is a host object: only the binding sees it'),
    Allowed('languages/python/monorm/admin.py', 'actions is a list', 'an action entry is a host function or a name: only the binding sees it'),
    Allowed('languages/python/monorm/admin.py', '{what}: {e}', 'the lowering of a host column, function or expression names the model and the place of a failure'),
    Allowed('languages/python/monorm/admin.py', 'queryset: {e}', 'the lowering of a host column, function or expression names the model and the place of a failure'),
    Allowed('languages/python/monorm/admin.py', 'the queryset function returns a condition', 'the binding calls the queryset function: only it sees the result'),
    Allowed('languages/python/monorm/admin.py', 'is not a file field', 'a host call at run time, not a config shape: the file registry is in the binding'),
    Allowed('languages/python/monorm/admin.py', 'an action is a name or a function', 'an action entry is a host function or a name: only the binding sees it'),
    Allowed('languages/python/monorm/admin.py', 'inputs is a list of admin.input', 'an input control is a host object: only the binding sees it'),
    Allowed('languages/js/src/admin.ts', 'files maps a field name', 'the `files` storage names stay in the binding, which owns the file storage registry'),
    Allowed('languages/js/src/admin.ts', 'is not a file field', 'a host call at run time, not a config shape: the file registry is in the binding'),
    Allowed('languages/js/src/admin.ts', '${where}: ${e instanceof Error', 'the lowering of a host column, function or expression names the place of a failure'),
    Allowed('languages/js/src/admin.ts', 'typeof value === "bigint"', 'a literal of the admin: only the binding sees the JavaScript type of the value'),
    Allowed('languages/js/src/admin.ts', 'a value of this type is not a literal', 'a literal of the admin: only the binding sees the JavaScript type of the value'),
    Allowed('languages/js/src/admin.ts', 'the queryset function returns a condition', 'the binding calls the queryset function: only it sees the result'),
    Allowed('languages/js/src/admin.ts', 'actions is a list', 'an action entry is a host function or a name: only the binding sees it'),
    Allowed('languages/js/src/admin.ts', 'an action is a name or a function', 'an action entry is a host function or a name: only the binding sees it'),
    Allowed('languages/js/src/admin.ts', 'an action needs a name', 'an action entry is a host function or a name: only the binding sees it'),
    Allowed('languages/js/src/admin.ts', 'inputs is a list of input(...)', 'an input control is a host object: only the binding sees it'),
    Allowed('languages/js/src/admin.ts', 'inlines is a list of inline(...)', 'an inline wrapper is a host object: only the binding sees it'),
    Allowed('languages/js/src/admin-cli.ts', 'e instanceof AdminConfigError', 'the command line maps a failure to the settings class, which sets the exit code 2'),
    Allowed('languages/js/src/admin-cli.ts', 'throw new AdminSettingsError(e instanceof Error', 'the command line maps a failure to the settings class, which sets the exit code 2'),
    Allowed('languages/js/src/admin.ts', 'new AdminSettingsError(err.message)', '`serve` maps a failure to the settings class, which sets the exit code 2'),
]

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


ERROR_CLASS = re.compile(r"\bnew\s+Admin(?:Config|Settings)Error\s*\(")


def python_error_sites(source: str, error_map: str | None) -> list[int]:
    """The lines that make an admin error class, outside the function of the error map."""
    tree = ast.parse(source)
    inside: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == error_map:
            inside.update(id(n) for n in ast.walk(node))
    names = {"AdminConfigError", "AdminSettingsError"}

    def makes_class(func: ast.expr) -> bool:
        if isinstance(func, ast.Name):
            return func.id in names
        return isinstance(func, ast.IfExp) and makes_class(func.body) and makes_class(func.orelse)

    return [n.lineno for n in ast.walk(tree) if isinstance(n, ast.Call) and id(n) not in inside and makes_class(n.func)]


def typescript_error_sites(source: str, error_map: str | None) -> list[int]:
    code = re.sub(r"/\*.*?\*/", lambda m: re.sub(r"[^\n]", " ", m[0]), source, flags=re.S)
    lines = [re.sub(r"(?<![:\"'`])//.*", "", line) for line in code.split("\n")]
    skip = range(0)
    if error_map and (m := re.search(rf"^(?:export )?function {error_map}\b", "\n".join(lines), re.M)):
        start = "\n".join(lines)[: m.start()].count("\n")
        depth, seen = 0, False
        for n in range(start, len(lines)):
            depth += lines[n].count("{") - lines[n].count("}")
            seen = seen or "{" in lines[n]
            if seen and depth <= 0:
                skip = range(start, n + 1)
                break
    return [n + 1 for n, line in enumerate(lines) if n not in skip and ERROR_CLASS.search(line)]


def scan_admin(root: Path, allowed: list[Allowed] = ADMIN_ALLOWED) -> list[str]:
    problems: list[str] = []
    used: set[Allowed] = set()
    for directory, name in ADMIN_FILES:
        for path in sorted((root / directory).glob("*")):
            if not name.search(path.name):
                continue
            rel = path.relative_to(root).as_posix()
            source = path.read_text(encoding="utf-8")
            sites = python_error_sites(source, ERROR_MAPS.get(rel)) if path.suffix == ".py" else typescript_error_sites(source, ERROR_MAPS.get(rel))
            text = source.split("\n")
            for line in sites:
                kept = next((a for a in allowed if a.path == rel and a.snippet in text[line - 1]), None)
                if kept:
                    used.add(kept)
                else:
                    problems.append(f"{rel}:{line}: an admin binding checks a config: {text[line - 1].strip()[:80]!r}")
    problems += [f"stale admin allow list entry: {a.path}: {a.snippet!r}" for a in allowed if a not in used]
    return problems


def main() -> int:
    root = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
    problems = scan(root) + scan_admin(root)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print("Move the SQL into monorm-core or monorm-engine, or add the site to ALLOWED with a reason.", file=sys.stderr)
        print("Move an admin shape check into crates/admin, or add the site to ADMIN_ALLOWED with a reason.", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
