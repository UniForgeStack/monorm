#!/usr/bin/env python3
"""Fail when a test case exists in one language and has no twin in the other.

A case ID is `<file stem>::<test name>`, normalized by `normalize`.
A Python test sets its own ID with `@pytest.mark.case("...")`.
A TypeScript test sets its own ID with a `// case: ...` comment on the line above.
`languages/test-twins.toml` lists the IDs and files that have no twin.
The check fails on a one-language ID that the file does not list.
"""

import ast
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PYTHON_DIR = ROOT / "languages/python/tests"
TYPESCRIPT_DIR = ROOT / "languages/js/test"
ALLOW_LIST = ROOT / "languages/test-twins.toml"

TS_TEST = re.compile(r"""^\s*(?:test|it)(?:\.\w+)?\(\s*(?P<q>["'`])(?P<name>.*?)(?P=q)\s*[,)]""")
TS_CASE = re.compile(r"^\s*//\s*case:\s*(?P<id>.+?)\s*$")
TS_HOLE = re.compile(r"\$\{[^}]*\}")


def normalize(text: str) -> str:
    """Lower case, with a camelCase word split and every run of non-alphanumerics as one underscore."""
    return re.sub(r"[^a-z0-9]+", "_", re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", text).lower()).strip("_")


def stem_of(name: str) -> str:
    """The file stem without the `test_` prefix or the `.test` suffix."""
    name = re.sub(r"\.(py|ts)$", "", name)
    name = re.sub(r"\.test$", "", name)
    return normalize(re.sub(r"^test_", "", name))


def case_id(stem: str, name: str) -> str:
    return f"{stem}::{normalize(name)}"


def explicit_id(stem: str, text: str) -> str:
    """An explicit ID is `<file stem>::<name>` or only a name in the file of the test."""
    if "::" in text:
        file_part, name = text.split("::", 1)
        return case_id(stem_of(file_part), name)
    return case_id(stem, text)


def _marker_id(decorator: ast.expr) -> str | None:
    if isinstance(decorator, ast.Call) and decorator.args and isinstance(decorator.args[0], ast.Constant):
        func = decorator.func
        if isinstance(func, ast.Attribute) and func.attr == "case":
            return str(decorator.args[0].value)
    return None


def python_cases(source: str, stem: str) -> set[str]:
    ids = set()
    for node in ast.walk(ast.parse(source)):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) or not node.name.startswith("test_"):
            continue
        marked = [m for m in map(_marker_id, node.decorator_list) if m]
        ids.add(explicit_id(stem, marked[0]) if marked else case_id(stem, node.name.removeprefix("test_")))
    return ids


def typescript_cases(source: str, stem: str) -> set[str]:
    ids = set()
    previous = ""
    for line in source.splitlines():
        match = TS_TEST.match(line)
        if match:
            declared = TS_CASE.match(previous)
            if declared:
                ids.add(explicit_id(stem, declared["id"]))
            else:
                # A name built from a loop variable keeps its fixed words: "(${provider})" and "${what}" drop out.
                ids.add(case_id(stem, TS_HOLE.sub(" ", match["name"])))
        previous = line
    return ids


def collect(files: list[Path], extract) -> dict[str, set[str]]:
    """The IDs of one language, with the files that declare each."""
    found: dict[str, set[str]] = {}
    for path in files:
        for cid in extract(path.read_text(), stem_of(path.name)):
            found.setdefault(cid, set()).add(path.relative_to(ROOT).as_posix())
    return found


def python_files() -> list[Path]:
    return sorted(PYTHON_DIR.glob("test_*.py"))


def typescript_files() -> list[Path]:
    return sorted(TYPESCRIPT_DIR.rglob("*.test.ts"))


def load_allow_list(path: Path) -> dict[str, dict[str, dict]]:
    """Per side, the `ids` and `files` tables of the allow list."""
    data = tomllib.loads(path.read_text()) if path.exists() else {}
    return {side: {"ids": dict(data.get(side, {}).get("ids", {})), "files": dict(data.get(side, {}).get("files", {}))} for side in ("python-only", "typescript-only")}


def is_listed(cid: str, files: set[str], side: dict[str, dict]) -> bool:
    return cid in side["ids"] or any(f in side["files"] for f in files)


def compare(python: dict[str, set[str]], typescript: dict[str, set[str]], allow: dict[str, dict[str, dict]]):
    """The matched IDs, and the one-language IDs split into listed and unlisted."""
    matched = python.keys() & typescript.keys()
    result = {"matched": sorted(matched)}
    for key, mine, other, side in (
        ("python", python, typescript, "python-only"),
        ("typescript", typescript, python, "typescript-only"),
    ):
        only = sorted(mine.keys() - other.keys())
        result[f"{key}_listed"] = [c for c in only if is_listed(c, mine[c], allow[side])]
        result[f"{key}_unlisted"] = [c for c in only if not is_listed(c, mine[c], allow[side])]
    return result


def stale_entries(python: dict[str, set[str]], typescript: dict[str, set[str]], allow) -> list[str]:
    """Listed IDs that now have a twin, or no test at all."""
    stale = []
    for side, mine, other in (("python-only", python, typescript), ("typescript-only", typescript, python)):
        for cid in allow[side]["ids"]:
            if cid not in mine:
                stale.append(f"{side}: {cid} has no test")
            elif cid in other:
                stale.append(f"{side}: {cid} has a twin now")
    return stale


def main() -> int:
    python = collect(python_files(), python_cases)
    typescript = collect(typescript_files(), typescript_cases)
    allow = load_allow_list(ALLOW_LIST)
    result = compare(python, typescript, allow)
    print(
        f"matched {len(result['matched'])}; "
        f"python-only {len(result['python_listed']) + len(result['python_unlisted'])}; "
        f"typescript-only {len(result['typescript_listed']) + len(result['typescript_unlisted'])}"
    )
    problems = [f"python only, not in {ALLOW_LIST.name}: {c}" for c in result["python_unlisted"]]
    problems += [f"typescript only, not in {ALLOW_LIST.name}: {c}" for c in result["typescript_unlisted"]]
    problems += stale_entries(python, typescript, allow)
    for line in problems:
        print(line, file=sys.stderr)
    if problems:
        print(f"Give the test a twin, name it like its twin, or list it in {ALLOW_LIST.relative_to(ROOT)} with a reason.", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
