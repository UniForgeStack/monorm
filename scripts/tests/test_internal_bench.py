"""Smoke test of bench/internal: run.sh --quick on SQLite, and on PostgreSQL when MONORM_BENCH_SMOKE_PG_URL names a disposable database. It asserts that each case and path has a number (only the driver row may be skipped, only the insert has no prepared path), never a timing assertion."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CASES = ["pk_get", "filter_order_50", "read_1000", "filtered_count", "insert_one", "prefetch_posts"]
PATHS = ["unprepared", "prepared", "direct", "driver"]
ADMIN_CASES = {"admin_list": ["admin", "driver"], "admin_export": ["admin", "host", "driver"]}
WRITE_CASES = {name: ["unprepared", "prepared", "driver"] for name in ("update_many_expr", "update_many_values")}


class _Quick:
    """The checks of one backend. A subclass sets `backend` and `database_url`, and `skipUnless` decides whether it runs."""

    backend = ""

    @classmethod
    def database_url(cls, tmp: str) -> str:
        raise NotImplementedError

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.out = Path(cls.tmp.name) / "out"
        env = {"ORM_BENCH_URL": cls.database_url(cls.tmp.name)}
        run = subprocess.run(
            [str(ROOT / "bench/internal/run.sh"), "--quick", "--out", str(cls.out)],
            capture_output=True,
            text=True,
            env={**os.environ, **env},
        )
        cls.run_result = run

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_run_succeeds_and_writes_the_summary(self):
        self.assertEqual(self.run_result.returncode, 0, self.run_result.stdout + self.run_result.stderr)
        self.assertTrue((self.out / "summary.md").exists())
        meta = json.loads((self.out / "run.json").read_text())
        self.assertTrue(meta["quick"])
        self.assertFalse(meta["valid"], "a quick run must never pass as a valid measurement")

    def test_every_case_and_path_has_a_number(self):
        for language in ("python", "js"):
            run = json.loads((self.out / f"{language}-{self.backend}.json").read_text())
            self.assertEqual(list(run["cases"]), [*CASES, *ADMIN_CASES, *WRITE_CASES])
            for case in CASES:
                entry = run["cases"][case]
                self.assertIs(entry["checked"], True, f"{language} {case}: the equality check did not run")
                self.assertEqual(sorted(entry["paths"]), sorted(PATHS))
                for path in PATHS:
                    result = entry["paths"][path]
                    if path == "driver" and result.get("skipped"):
                        continue
                    if path == "prepared" and case == "insert_one":
                        self.assertTrue(result.get("na"), f"{language} {case}: no reason for the missing prepared path")
                        continue
                    self.assertIn("median_us", result, f"{language} {case} {path}: no number")
                    self.assertGreater(result["median_us"], 0)

    def test_the_admin_and_write_cases_have_a_number_on_each_path(self):
        for language in ("python", "js"):
            run = json.loads((self.out / f"{language}-{self.backend}.json").read_text())
            for case, paths in {**ADMIN_CASES, **WRITE_CASES}.items():
                entry = run["cases"][case]
                self.assertIs(entry["checked"], True, f"{language} {case}: the equality check did not run")
                self.assertEqual(sorted(entry["paths"]), sorted(paths))
                for path in paths:
                    result = entry["paths"][path]
                    if path == "driver" and result.get("skipped"):
                        continue
                    self.assertIn("median_us", result, f"{language} {case} {path}: no number")
                    self.assertGreater(result["median_us"], 0)

    def test_the_summary_has_the_admin_table(self):
        summary = (self.out / "summary.md").read_text()
        self.assertIn("| admin_list |", summary)
        self.assertIn("| admin_export |", summary)

    def test_only_the_insert_has_no_prepared_path(self):
        for language in ("python", "js"):
            run = json.loads((self.out / f"{language}-{self.backend}.json").read_text())
            for case in CASES:
                prepared = run["cases"][case]["paths"]["prepared"]
                self.assertEqual("na" in prepared, case == "insert_one", f"{language} {case}")


# It builds release artifacts for minutes, so pytest skips it unless asked (the CLAUDE.md check line sets the variable).
@unittest.skipUnless(os.environ.get("MONORM_BENCH_SMOKE") == "1", "set MONORM_BENCH_SMOKE=1 to run bench/internal/run.sh --quick")
class InternalBenchQuickSqlite(_Quick, unittest.TestCase):
    backend = "sqlite"

    @classmethod
    def database_url(cls, tmp: str) -> str:
        return f"sqlite://{tmp}/bench.db"


# The database must be disposable: the bench drops and creates its tables.
@unittest.skipUnless(os.environ.get("MONORM_BENCH_SMOKE_PG_URL"), "set MONORM_BENCH_SMOKE_PG_URL to a disposable PostgreSQL database to run bench/internal/run.sh --quick on it")
class InternalBenchQuickPostgresql(_Quick, unittest.TestCase):
    backend = "postgresql"

    @classmethod
    def database_url(cls, tmp: str) -> str:
        return os.environ["MONORM_BENCH_SMOKE_PG_URL"]


if __name__ == "__main__":
    unittest.main()
