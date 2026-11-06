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
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / "fixtures/docs"
OPTIONS = {"setup": None, "db": {"sqlite", "postgresql"}}
POSTGRESQL_SKIPPED = "ORM_DOCS_DATABASE_URL is unset: PostgreSQL doc skipped"
DEFAULT_TEST_URL = "postgres://postgres:postgres@localhost/orm_test"
FENCE = re.compile(r"\s*(`{3,}|~{3,})(.*)")


def parse_fences(source):
    blocks = []
    opening, opened, block, body = None, 0, {}, []
    for line_number, line in enumerate(re.split(r"\r?\n", source), 1):
        if opening is None:
            match = FENCE.fullmatch(line)
            if match and not (match[1][0] == "`" and "`" in match[2]):
                words = match[2].split()
                if match[1][0] == "~" and "run" in words:
                    raise ValueError(f"line {line_number}: a ~~~ fence cannot run; use ```")
                opening, opened = match[1], line_number
                block = {"info": words, "line": line_number + 1, "source": ""}
                body = []
        elif line.strip() == opening:
            block["source"] = "\n".join(body) + "\n"
            blocks.append(block)
            opening = None
        else:
            body.append(line)
    if opening is not None:
        raise ValueError(f"line {opened}: unclosed fence")
    return blocks


def options(source):
    match = re.search(r"<!--\s*docs-test:\s*(.*?)\s*-->", source)
    config = {}
    for word in match[1].split() if match else []:
        key, separator, value = word.partition("=")
        if key not in OPTIONS or not separator or not value:
            raise ValueError(f"docs-test: unknown option {word!r}")
        if OPTIONS[key] and value not in OPTIONS[key]:
            raise ValueError(f"docs-test: {key} must be one of {sorted(OPTIONS[key])}, not {value!r}")
        config[key] = value
    return config


def first_line(source):
    return source.split("\n")[0] or "<empty>"


def is_run(block, language):
    return block["info"][:1] == [language] and "run" in block["info"]


def database_key(url):
    parts = urlsplit(url)
    return parts.hostname, parts.port or 5432, parts.path


def database_url(config, env):
    if config.get("db") != "postgresql":
        return "sqlite://:memory:"
    url = env["ORM_DOCS_DATABASE_URL"]
    if database_key(url) == database_key(env.get("ORM_TEST_DATABASE_URL", DEFAULT_TEST_URL)):
        raise ValueError("ORM_DOCS_DATABASE_URL must not be the test database: the doc drops its tables")
    return url


def skip_reason(doc):
    if options(doc.read_text()).get("db") == "postgresql" and not os.environ.get("ORM_DOCS_DATABASE_URL"):
        return POSTGRESQL_SKIPPED


async def execute(source, namespace, filename):
    result = eval(compile(source, filename, "exec", flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT), namespace)
    if inspect.isawaitable(result):
        await result


async def run_doc(doc):
    source = doc.read_text()
    config = options(source)
    url = database_url(config, os.environ)
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
                namespace.update(docs_schema=schema, docs_database_url=url)
                await execute((FIXTURES / f"{setup}.py").read_text(), namespace, label)
            for block in blocks:
                if not is_run(block, "python"):
                    continue
                label = f"{doc}:{block['line']}: {first_line(block['source'])}"
                expected = next((word[5:] for word in block["info"] if word.startswith("fail=")), None)
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
            if cleanup := namespace.get("docs_test_cleanup"):
                await cleanup()


def child(doc, env=None):
    return subprocess.run([sys.executable, "-I", str(Path(__file__).resolve()), str(doc)],
                          capture_output=True, text=True, timeout=60, env={**os.environ, **(env or {})})


if __name__ == "__main__":
    asyncio.run(run_doc(Path(sys.argv[1]).resolve()))
else:
    import pytest

    DOCS = [path for path in sorted((ROOT / "docs").rglob("*.md"))
            if any(is_run(block, "python") for block in parse_fences(path.read_text()))]

    @pytest.mark.parametrize("doc", DOCS + [FIXTURES / "execution-cases.md", FIXTURES / "postgresql.md"],
                             ids=lambda path: str(path.relative_to(ROOT)))
    def test_runnable_doc_blocks_run(doc):
        if reason := skip_reason(doc):
            pytest.skip(reason)
        result = child(doc)
        assert result.returncode == 0, result.stdout + result.stderr

    def test_the_fence_parser_reads_the_fixture():
        assert parse_fences((FIXTURES / "parser-cases.md").read_text()) == json.loads(
            (FIXTURES / "parser-cases.json").read_text())

    def test_the_fence_parser_reads_the_source_cases():
        for case in json.loads((FIXTURES / "source-cases.json").read_text()):
            if "error" in case:
                with pytest.raises(ValueError, match=re.escape(case["error"])):
                    parse_fences(case["source"])
            else:
                assert parse_fences(case["source"]) == case["blocks"], case["name"]

    def test_the_option_parser_reads_the_fixture():
        for case in json.loads((FIXTURES / "option-cases.json").read_text()):
            if "error" in case:
                with pytest.raises(ValueError, match=re.escape(case["error"])):
                    options(case["source"])
            else:
                assert options(case["source"]) == case["options"], case["source"]

    def test_a_failing_block_names_the_doc_and_the_block():
        doc = FIXTURES / "broken.md"
        result = child(doc)
        assert result.returncode != 0
        assert str(doc) in result.stderr
        assert "await User.objects.no_such_method()" in result.stderr
        with tempfile.TemporaryDirectory() as directory:
            for body, message in [("pass", "expected ValueError, block passed"),
                                  ("raise TypeError('wrong error')", "wrong error"),
                                  ("def (:", ":2: def (:")]:
                expected_doc = Path(directory) / "expected-error.md"
                expected_doc.write_text(f"```python run fail=ValueError\n{body}\n```\n")
                result = child(expected_doc)
                assert result.returncode != 0
                assert str(expected_doc) in result.stderr
                assert message in result.stderr

    def test_a_doc_database_cannot_be_the_test_database():
        url = "postgres://postgres:postgres@localhost/shared"
        result = child(FIXTURES / "postgresql.md", {"ORM_DOCS_DATABASE_URL": url, "ORM_TEST_DATABASE_URL": url})
        assert result.returncode != 0
        assert "must not be the test database" in result.stderr
