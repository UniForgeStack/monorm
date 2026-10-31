"""Test the statement filter, the comment skipping and the allow list of scripts/check-thin-bindings.py."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "check-thin-bindings.py"
spec = importlib.util.spec_from_file_location("check_thin_bindings", SCRIPT)
thin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(thin)


def texts(source: str, suffix: str) -> list[str]:
    if suffix == ".py":
        return [t for _, t in thin.python_literals(source)]
    return [t for _, t in thin.scan_literals(source, suffix == ".rs")]


class StatementTest(unittest.TestCase):
    def test_statements_are_found(self) -> None:
        for sql in (
            "SELECT id FROM users",
            "select_all: SELECT * FROM t",
            "SELECT count(*) FROM (x)",
            "INSERT INTO t (a) VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t WHERE id = 1",
            "COPY t FROM STDIN",
            "SAVEPOINT sp1",
            "RELEASE SAVEPOINT sp1",
            "BEGIN",
            "BEGIN IMMEDIATE",
            "COMMIT;",
            "LOCK TABLE t IN SHARE MODE",
            "SET LOCAL app.tenant = 1",
            "DROP TABLE t CASCADE",
            "PRAGMA foreign_keys = ON",
            "WITH RECURSIVE r AS (SELECT 1)",
        ):
            self.assertTrue(thin.is_statement(sql), sql)

    def test_prose_is_not_a_statement(self) -> None:
        for text in (
            "select exactly one named native profile",
            "insert_many(copy=True) takes no batch_size: COPY is one statement",
            "lock key must fit in 64 bits",
            "UPDATE needs a where clause",
            "BEGIN is a keyword",
            "the SELECT this query runs",
            "Postgres COPY stops at the first conflict",
            "",
        ):
            self.assertFalse(thin.is_statement(text), text)


class LiteralTest(unittest.TestCase):
    def test_python_skips_docstrings_and_comments(self) -> None:
        source = '"""SELECT 1 FROM docs"""\n# SELECT 2 FROM comment\ndef f():\n    """INSERT INTO docs VALUES (1)"""\n    return "DELETE FROM t"\n'
        self.assertEqual(texts(source, ".py"), ["DELETE FROM t"])

    def test_python_reads_f_string_text(self) -> None:
        self.assertIn("SELECT x FROM ", texts('q = f"SELECT x FROM {t}"\n', ".py")[0])

    def test_typescript_skips_comments_and_reads_templates(self) -> None:
        source = '// SELECT a FROM b\n/** SELECT c FROM d */\nconst q = `DELETE FROM ${t}`;\nconst u = "http://x"; // tail\n'
        self.assertEqual(texts(source, ".ts"), ["DELETE FROM ${t}", "http://x"])

    def test_rust_skips_comments_and_reads_raw_strings(self) -> None:
        source = '/// SELECT a FROM b\n//! SELECT c FROM d\nfn f() { let q = r#"INSERT INTO "t" VALUES (1)"#; let c = \'"\'; let s: &\'static str = "x"; }\n'
        self.assertEqual(texts(source, ".rs"), ['INSERT INTO "t" VALUES (1)', "x"])

    def test_rust_nested_block_comment(self) -> None:
        self.assertEqual(texts('/* a /* SELECT 1 FROM t */ b */ let x = "ok";', ".rs"), ["ok"])


class ScanTest(unittest.TestCase):
    def make_root(self, files: dict[str, str]) -> Path:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(root, ignore_errors=True))
        for name, text in files.items():
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text(text)
        for directory, _ in thin.SCANNED:
            (root / directory).mkdir(parents=True, exist_ok=True)
        return root

    def test_clean_tree_passes(self) -> None:
        root = self.make_root({"languages/python/monorm/a.py": '"""SELECT 1 FROM t"""\nx = 1\n'})
        self.assertEqual(thin.scan(root, []), [])

    def test_sql_in_each_directory_fails(self) -> None:
        root = self.make_root(
            {
                "languages/python/monorm/a.py": 'q = "SELECT 1 FROM t"\n',
                "languages/js/src/a.ts": "const q = `DELETE FROM t`;\n",
                "languages/python/native/src/a.rs": 'fn f() { let q = "BEGIN"; }\n',
                "languages/js/native/src/a.rs": 'fn f() { let q = "COMMIT"; }\n',
            }
        )
        problems = thin.scan(root, [])
        self.assertEqual(len(problems), 4, problems)
        self.assertIn("languages/js/src/a.ts:1:", " ".join(problems))

    def test_allow_list_keeps_a_site_and_flags_a_stale_entry(self) -> None:
        root = self.make_root({"languages/js/src/a.ts": "const q = `DELETE FROM t`;\n"})
        kept = thin.Allowed("languages/js/src/a.ts", "DELETE FROM", "a reason")
        self.assertEqual(thin.scan(root, [kept]), [])
        stale = thin.Allowed("languages/js/src/a.ts", "INSERT INTO", "a reason")
        self.assertEqual(len(thin.scan(root, [kept, stale])), 1)

    def test_the_repository_passes(self) -> None:
        result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
