"""Smoke test of bench/internal: run.sh --quick on SQLite. It asserts that each case and path has a number (only the driver row may be skipped, only the insert has no prepared path), never a timing assertion."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CASES = ["pk_get", "filter_order_50", "read_1000", "filtered_count", "insert_one", "prefetch_posts"]
PATHS = ["unprepared", "prepared", "direct", "driver"]


# It builds release artifacts for minutes, so pytest skips it unless asked (the CLAUDE.md check line sets the variable).
@unittest.skipUnless(os.environ.get("MONORM_BENCH_SMOKE") == "1", "set MONORM_BENCH_SMOKE=1 to run bench/internal/run.sh --quick")
class InternalBenchQuick(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.out = Path(cls.tmp.name) / "out"
        env = {"ORM_BENCH_URL": f"sqlite://{cls.tmp.name}/bench.db"}
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
            run = json.loads((self.out / f"{language}-sqlite.json").read_text())
            self.assertEqual(list(run["cases"]), CASES)
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

    def test_only_the_insert_has_no_prepared_path(self):
        for language in ("python", "js"):
            run = json.loads((self.out / f"{language}-sqlite.json").read_text())
            for case in CASES:
                prepared = run["cases"][case]["paths"]["prepared"]
                self.assertEqual("na" in prepared, case == "insert_one", f"{language} {case}")


if __name__ == "__main__":
    unittest.main()
