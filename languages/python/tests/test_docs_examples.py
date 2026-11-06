"""Opt-in Markdown examples, isolated from pytest's default model registry."""

import ast
import asyncio
import inspect
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "fixtures/docs"


def parse_fences(source):
    blocks = []
    opening = None
    for line_number, line in enumerate(source.splitlines(), 1):
        if opening is None:
            match = re.fullmatch(r"\s*(`{3,})([^`]*)", line)
            if match:
                opening = match[1]
                words = match[2].split()
                block = {"info": words, "line": line_number + 1, "source": ""}
                body = []
        elif line.strip() == opening:
            block["source"] = "\n".join(body) + "\n"
            blocks.append(block)
            opening = None
        else:
            body.append(line)
    return blocks


def options(source):
    match = re.search(r"<!--\s*docs-test:\s*(.*?)\s*-->", source)
    return dict(word.split("=", 1) for word in match[1].split()) if match else {}


async def execute(source, namespace, filename):
    result = eval(compile(source, filename, "exec", flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT), namespace)
    if inspect.isawaitable(result):
        await result


async def run_doc(doc):
    source = doc.read_text()
    config = options(source)
    if config.get("db", "sqlite") == "postgresql" and not os.environ.get("ORM_DOCS_DATABASE_URL"):
        print("ORM_DOCS_DATABASE_URL is unset: PostgreSQL doc skipped")
        return 77
    blocks = parse_fences(source)
    namespace = {"__name__": "__docs_example__"}
    with tempfile.TemporaryDirectory(prefix="monorm-docs-") as directory:
        os.chdir(directory)
        for block in blocks:
            for word in block["info"]:
                if word.startswith("file="):
                    target = Path(directory) / word[5:]
                    if not target.resolve().is_relative_to(Path(directory).resolve()):
                        raise ValueError("file= must stay inside the doc work directory")
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(block["source"])
        label = f"{doc}: setup"
        try:
            if setup := config.get("setup"):
                schema = (FIXTURES / f"{setup}.prisma").read_text()
                if config.get("db") == "postgresql":
                    schema = schema.replace('provider = "sqlite"', 'provider = "postgresql"')
                namespace.update(docs_schema=schema, docs_database_url=(
                    os.environ["ORM_DOCS_DATABASE_URL"] if config.get("db") == "postgresql" else "sqlite://:memory:"
                ))
                await execute((FIXTURES / f"{setup}.py").read_text(), namespace, label)
            for block in blocks:
                info = block["info"]
                if not info or info[0] != "python" or "run" not in info:
                    continue
                first = block["source"].splitlines()[0] if block["source"].splitlines() else "<empty>"
                label = f"{doc}:{block['line']}: {first}"
                expected = next((word[5:] for word in info if word.startswith("fail=")), None)
                try:
                    await execute(block["source"], namespace, label)
                except Exception as error:
                    if expected != type(error).__name__:
                        raise
                else:
                    if expected:
                        raise AssertionError(f"expected {expected}, block passed")
        except Exception as error:
            raise RuntimeError(f"{label}\n{error}") from error
        finally:
            if database := namespace.get("db"):
                try:
                    await database.drop_tables()
                finally:
                    await database.close()
    return 0


def child(doc):
    return subprocess.run([sys.executable, "-I", str(Path(__file__).resolve()), str(doc)],
                          capture_output=True, text=True, timeout=60)


if __name__ == "__main__":
    sys.exit(asyncio.run(run_doc(Path(sys.argv[1]).resolve())))
else:
    import pytest

    DOCS = [path for path in sorted((ROOT / "docs").rglob("*.md"))
            if any("run" in block["info"] and block["info"][0] == "python"
                   for block in parse_fences(path.read_text()))]

    @pytest.mark.parametrize("doc", DOCS + [FIXTURES / "execution-cases.md", FIXTURES / "postgresql.md"],
                             ids=lambda path: str(path.relative_to(ROOT)))
    def test_runnable_doc_blocks_run(doc):
        result = child(doc)
        if result.returncode == 77:
            pytest.skip(result.stdout.strip())
        assert result.returncode == 0, result.stdout + result.stderr

    def test_the_fence_parser_reads_the_fixture():
        assert parse_fences((FIXTURES / "parser-cases.md").read_text()) == json.loads(
            (FIXTURES / "parser-cases.json").read_text())

    def test_a_failing_block_names_the_doc_and_the_block():
        doc = FIXTURES / "broken.md"
        result = child(doc)
        assert result.returncode != 0
        assert str(doc) in result.stderr
        assert "await User.objects.no_such_method()" in result.stderr
        with tempfile.TemporaryDirectory() as directory:
            for body, message in [("pass", "expected ValueError, block passed"),
                                  ("raise TypeError('wrong error')", "wrong error")]:
                expected_doc = Path(directory) / "expected-error.md"
                expected_doc.write_text(f"```python run fail=ValueError\n{body}\n```\n")
                result = child(expected_doc)
                assert result.returncode != 0
                assert str(expected_doc) in result.stderr
                assert message in result.stderr
