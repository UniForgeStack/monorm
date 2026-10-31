"""Test the ID rules, the explicit IDs, the allow list and the ratchet of scripts/check-test-twins.py."""

import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "check-test-twins.py"
spec = importlib.util.spec_from_file_location("check_test_twins", SCRIPT)
twins = importlib.util.module_from_spec(spec)
spec.loader.exec_module(twins)

NO_ALLOW = {side: {"ids": {}, "files": {}} for side in ("python-only", "typescript-only")}


def allow(python_ids=(), typescript_ids=(), python_files=(), typescript_files=()):
    return {
        "python-only": {"ids": {i: "r" for i in python_ids}, "files": {f: "r" for f in python_files}},
        "typescript-only": {"ids": {i: "r" for i in typescript_ids}, "files": {f: "r" for f in typescript_files}},
    }


class NormalizeTest(unittest.TestCase):
    def test_runs_of_non_alphanumerics_become_one_underscore(self):
        self.assertEqual(twins.normalize("A bound read -- ignores (root) defaults!"), "a_bound_read_ignores_root_defaults")

    def test_a_camel_case_word_splits_like_a_snake_case_word(self):
        self.assertEqual(twins.normalize("inBulk and DoesNotExist in SQLite"), "in_bulk_and_does_not_exist_in_sqlite")

    def test_stem_drops_the_test_prefix_and_the_test_suffix(self):
        self.assertEqual(twins.stem_of("test_bound_reads.py"), "bound_reads")
        self.assertEqual(twins.stem_of("bound-reads.test.ts"), "bound_reads")

    def test_a_python_name_and_a_typescript_name_meet(self):
        python = twins.python_cases("def test_a_bound_read_works():\n    pass\n", "bound_reads")
        typescript = twins.typescript_cases('test("a bound read works", () => {});\n', "bound_reads")
        self.assertEqual(python, typescript)


class ExtractTest(unittest.TestCase):
    def test_python_finds_async_functions_and_methods_and_counts_a_parametrized_case_once(self):
        source = (
            "import pytest\n"
            "@pytest.mark.parametrize('x', [1, 2, 3])\n"
            "def test_each(x):\n    pass\n"
            "async def test_later():\n    pass\n"
            "class TestGroup:\n    def test_inside(self):\n        pass\n"
            "def helper_test():\n    pass\n"
        )
        self.assertEqual(twins.python_cases(source, "f"), {"f::each", "f::later", "f::inside"})

    def test_python_marker_sets_the_id(self):
        source = "@pytest.mark.case('Other name')\ndef test_clumsy_name():\n    pass\n"
        self.assertEqual(twins.python_cases(source, "f"), {"f::other_name"})

    def test_python_marker_can_name_another_file(self):
        source = "@pytest.mark.case('sql.test::shared case')\ndef test_x():\n    pass\n"
        self.assertEqual(twins.python_cases(source, "f"), {"sql::shared_case"})

    def test_typescript_finds_test_it_and_modifiers(self):
        source = 'test("one", () => {});\n  it("two", async () => {});\ntest.skip("three", () => {});\n// test("four")\n'
        self.assertEqual(twins.typescript_cases(source, "f"), {"f::one", "f::two", "f::three"})

    def test_typescript_template_holes_drop_out_and_a_loop_counts_once(self):
        source = "for (const p of ps) {\n  test(`crud works (${p})`, async () => {});\n}\n"
        self.assertEqual(twins.typescript_cases(source, "f"), {"f::crud_works"})

    def test_typescript_case_comment_sets_the_id(self):
        source = '// case: Shared name\ntest("a clumsy sentence", () => {});\ntest("plain", () => {});\n'
        self.assertEqual(twins.typescript_cases(source, "f"), {"f::shared_name", "f::plain"})

    def test_typescript_options_after_the_name_are_ignored(self):
        self.assertEqual(twins.typescript_cases('test("named", { skip: true }, async () => {});\n', "f"), {"f::named"})


class CompareTest(unittest.TestCase):
    python = {"f::both": {"py/f.py"}, "f::py": {"py/f.py"}}
    typescript = {"f::both": {"ts/f.ts"}, "f::ts": {"ts/f.ts"}}

    def test_an_unlisted_one_language_id_is_reported(self):
        result = twins.compare(self.python, self.typescript, NO_ALLOW)
        self.assertEqual(result["matched"], ["f::both"])
        self.assertEqual(result["python_unlisted"], ["f::py"])
        self.assertEqual(result["typescript_unlisted"], ["f::ts"])

    def test_an_id_entry_lists_a_case(self):
        result = twins.compare(self.python, self.typescript, allow(python_ids=["f::py"]))
        self.assertEqual((result["python_listed"], result["python_unlisted"]), (["f::py"], []))
        self.assertEqual(result["typescript_unlisted"], ["f::ts"])

    def test_a_file_entry_lists_every_case_of_the_file(self):
        result = twins.compare(self.python, self.typescript, allow(typescript_files=["ts/f.ts"]))
        self.assertEqual(result["typescript_unlisted"], [])

    def test_an_entry_of_the_other_side_does_not_list_a_case(self):
        result = twins.compare(self.python, self.typescript, allow(typescript_ids=["f::py"]))
        self.assertEqual(result["python_unlisted"], ["f::py"])

    def test_stale_entries_are_a_case_with_a_twin_and_a_case_that_is_gone(self):
        entries = allow(python_ids=["f::both", "f::gone"])
        stale = twins.stale_entries(self.python, self.typescript, entries)
        self.assertEqual(len(stale), 2)
        self.assertTrue(any("f::both has a twin now" in s for s in stale))
        self.assertTrue(any("f::gone has no test" in s for s in stale))


class RatchetTest(unittest.TestCase):
    """Run the script on a fake tree: the old unreviewed IDs pass, a new ID fails."""

    def tree(self, files: dict[str, str]) -> Path:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        (root / "scripts").mkdir()
        shutil.copy(SCRIPT, root / "scripts/check-test-twins.py")
        for name, text in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return root

    def run_script(self, root: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run([sys.executable, str(root / "scripts/check-test-twins.py")], capture_output=True, text=True)

    PAIR = {
        "languages/python/tests/test_a.py": "def test_both():\n    pass\n",
        "languages/js/test/a.test.ts": 'test("both", () => {});\n',
    }

    def test_a_matched_pair_passes_without_an_allow_list(self):
        result = self.run_script(self.tree(self.PAIR))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_listed_old_case_passes_and_a_new_case_fails(self):
        files = {
            **self.PAIR,
            "languages/python/tests/test_a.py": "def test_both():\n    pass\ndef test_old():\n    pass\n",
            "languages/test-twins.toml": '[python-only.ids]\n"a::old" = "unreviewed"\n',
        }
        root = self.tree(files)
        self.assertEqual(self.run_script(root).returncode, 0)
        (root / "languages/python/tests/test_a.py").write_text("def test_both():\n    pass\ndef test_old():\n    pass\ndef test_new():\n    pass\n")
        result = self.run_script(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("a::new", result.stderr)
        self.assertNotIn("a::old", result.stderr)

    def test_a_new_typescript_case_fails(self):
        root = self.tree(self.PAIR)
        (root / "languages/js/test/a.test.ts").write_text('test("both", () => {});\ntest("extra", () => {});\n')
        result = self.run_script(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("typescript only", result.stderr)

    def test_the_real_tree_passes(self):
        result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
