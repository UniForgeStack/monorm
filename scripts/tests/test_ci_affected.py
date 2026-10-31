"""scripts/ci/affected.py picks the check groups from the changed paths."""

import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "ci" / "affected.py"
spec = importlib.util.spec_from_file_location("affected", SCRIPT)
affected = importlib.util.module_from_spec(spec)
spec.loader.exec_module(affected)

REPO = SCRIPT.parents[2]


def plan(*files: str) -> set[str]:
    return affected.plan(list(files))[0]


class PlanTest(unittest.TestCase):
    def test_empty_diff_runs_only_the_name_checks(self) -> None:
        self.assertEqual(plan(), {"names"})

    def test_docs_only(self) -> None:
        self.assertEqual(plan("README.md", "docs/guides/x.md", "CONTRIBUTING.md"), {"names", "docs"})

    def test_changelog_also_feeds_the_release_check(self) -> None:
        self.assertEqual(plan("CHANGELOG.md"), {"names", "docs", "release-scripts"})

    def test_python_only(self) -> None:
        groups = plan("languages/python/monorm/client.py")
        self.assertEqual(groups, {"names", "rust", "python", "packages"})

    def test_python_tests_run_the_twin_check(self) -> None:
        self.assertEqual(plan("languages/python/tests/test_x.py"), {"names", "python", "test-twins"})

    def test_twin_check_files_run_the_twin_check(self) -> None:
        for path in ("scripts/check-test-twins.py", "scripts/tests/test_check_test_twins.py", "languages/test-twins.toml"):
            self.assertEqual(plan(path), {"names", "test-twins", "python"}, path)

    def test_thin_bindings_check_files_run_python(self) -> None:
        for path in ("scripts/check-thin-bindings.py", "scripts/tests/test_check_thin_bindings.py"):
            self.assertEqual(plan(path), {"names", "python"}, path)

    def test_typescript_only(self) -> None:
        self.assertEqual(plan("languages/js/src/client.ts"), {"names", "node", "create-monorm", "packages"})
        self.assertEqual(plan("languages/js/test/x.test.ts"), {"names", "node", "test-twins"})

    def test_create_monorm_only(self) -> None:
        self.assertEqual(plan("languages/js/create-monorm/bin.js"), {"names", "create-monorm"})

    def test_one_extension_marks_both_languages_and_the_packages(self) -> None:
        groups = plan("extensions/inherit/src/lib.rs")
        self.assertEqual(groups, {"names", "rust", "python", "node", "packages"})

    def test_schema_docs_run_the_documented_schema_test(self) -> None:
        self.assertEqual(plan("docs/schema/prisma-syntax.md"), {"names", "docs", "rust"})
        self.assertEqual(plan("docs/schema/schema.md"), {"names", "docs", "rust"})

    def test_python_package_runs_the_cargo_typing_tests(self) -> None:
        for path in ["languages/python/monorm/client.py", "languages/python/monorm/py.typed", "languages/python/packaging/stage.py"]:
            self.assertIn("rust", plan(path), path)
        self.assertNotIn("rust", plan("languages/python/tests/test_x.py"))

    def test_typescript_sources_the_cargo_tests_compile_run_rust(self) -> None:
        for path in ["languages/js/src/index.ts", "languages/js/src/ext.ts", "languages/js/package.json"]:
            self.assertEqual(plan(path) & {"rust", "node", "create-monorm", "packages"}, {"rust", "node", "create-monorm", "packages"}, path)
        self.assertNotIn("rust", plan("languages/js/src/client.ts"))

    def test_extension_package_crate_feeds_npm_assemble(self) -> None:
        self.assertGreaterEqual(plan("crates/extension-package/src/main.rs"), {"rust", "packages", "python", "node", "create-monorm"})

    def test_every_version_site_runs_the_release_check(self) -> None:
        paths = [
            "crates/cli/Cargo.toml",
            "languages/python/native/Cargo.toml",
            "languages/js/native/Cargo.toml",
            "extensions/inherit/monorm-extension.toml",
            "languages/js/create-monorm/package.json",
            "languages/js/src/native.ts",
            "tests/outside-extension/Cargo.lock",
        ]
        for path in paths:
            self.assertIn("release-scripts", plan(path), path)
        # A new site in versions.py needs a path here and in VERSION_SITES.
        text = (REPO / "scripts/release/versions.py").read_text()
        for path in re.findall(r'^    \("([^"]+)", "(?:py|semver)"', text, re.M):
            if path not in ("Cargo.lock", "pyproject.toml"):
                self.assertIn("release-scripts", plan(path.replace("*", "inherit")), path)

    def test_core_crate_selects_every_group(self) -> None:
        groups, reason = affected.plan(["crates/core/src/lib.rs"])
        self.assertEqual(groups, affected.BASE_GROUPS)
        self.assertIn("every group", reason)

    def test_engine_contracts_and_lockfile_select_every_group(self) -> None:
        for path in ["crates/engine/src/a.rs", "crates/contracts/src/a.rs", "Cargo.lock", "Cargo.toml", "pyproject.toml"]:
            self.assertGreaterEqual(plan(path), affected.BASE_GROUPS, path)

    def test_unknown_path_selects_every_group(self) -> None:
        groups, reason = affected.plan(["mystery/file.txt", "README.md"])
        self.assertGreaterEqual(groups, affected.BASE_GROUPS)
        self.assertIn("mystery/file.txt", reason)

    def test_workflow_and_ci_script_select_every_group(self) -> None:
        for path in [".github/workflows/checks.yml", "scripts/ci/affected.py", ".github/actions/affected/action.yml"]:
            self.assertGreaterEqual(plan(path), affected.BASE_GROUPS, path)

    def test_groups_of_several_paths_add_up(self) -> None:
        groups = plan("docs/a.md", "scripts/release/versions.py")
        self.assertEqual(groups, {"names", "docs", "release-scripts", "python"})

    def test_every_rule_names_a_known_group(self) -> None:
        for pattern, groups in affected.RULES:
            self.assertLessEqual(groups, affected.BASE_GROUPS, pattern)

    def test_every_job_gate_of_the_workflows_names_a_known_group(self) -> None:
        for name in ["checks.yml", "extension-packages.yml"]:
            text = (REPO / ".github/workflows" / name).read_text()
            gates = set(re.findall(r"contains\(fromJSON\(needs\.affected\.outputs\.groups\), '([^']+)'\)", text))
            self.assertTrue(gates, name)
            self.assertLessEqual(gates, affected.BASE_GROUPS, name)


class GitTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name)
        self.git("init", "-q", "-b", "main")
        self.commit("a.md", "one")

    def git(self, *args: str) -> str:
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t", "PATH": os.environ["PATH"]}
        done = subprocess.run(["git", *args], cwd=self.repo, capture_output=True, text=True, env=env, check=True)
        return done.stdout.strip()

    def commit(self, name: str, text: str) -> str:
        (self.repo / name).write_text(text)
        self.git("add", name)
        self.git("commit", "-q", "-m", name)
        return self.git("rev-parse", "HEAD")

    def changed(self, base: str, head: str, merge_base: bool = False) -> list[str] | str:
        original = affected.ROOT
        affected.ROOT = self.repo
        try:
            return affected.changed_files(base, head, merge_base)
        finally:
            affected.ROOT = original

    def test_all_commits_of_a_push_count(self) -> None:
        base = self.git("rev-parse", "HEAD")
        self.commit("b.md", "x")
        head = self.commit("c.md", "y")
        self.assertEqual(self.changed(base, head), ["b.md", "c.md"])

    def test_new_branch_and_unknown_base_have_no_diff(self) -> None:
        head = self.git("rev-parse", "HEAD")
        self.assertIsInstance(self.changed("0" * 40, head), str)
        self.assertIsInstance(self.changed("", head), str)
        self.assertIsInstance(self.changed("1" * 40, head), str)

    def test_forced_push_has_no_diff_but_a_pull_request_does(self) -> None:
        old = self.commit("b.md", "x")
        self.git("reset", "-q", "--hard", "HEAD~1")
        head = self.commit("c.md", "y")
        self.assertIsInstance(self.changed(old, head), str)
        self.assertEqual(self.changed(old, head, merge_base=True), ["c.md"])


class LinksTest(unittest.TestCase):
    def test_broken_relative_link_is_reported(self) -> None:
        links = importlib.util.module_from_spec(importlib.util.spec_from_file_location("links", SCRIPT.with_name("check_links.py")))
        links.__spec__.loader.exec_module(links)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "a.md").write_text("[ok](b.md#top) [web](https://x.y) `[code](no.md)`\n[bad](missing.md)\n```\n[fenced](no.md)\n```\n")
            (root / "b.md").write_text("")
            links.ROOT = root
            self.assertEqual(links.broken_links("a.md"), ["a.md:2: missing.md"])


class CliTest(unittest.TestCase):
    def run_cli(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True)

    def test_all_flag_prints_every_group_as_json(self) -> None:
        out = json.loads(self.run_cli("--all").stdout)
        self.assertTrue(out["all"])
        self.assertGreaterEqual(set(out["groups"]), affected.BASE_GROUPS)

    def test_empty_diff_prints_lines(self) -> None:
        self.assertEqual(self.run_cli("HEAD", "HEAD", "--format", "lines").stdout.split(), ["names"])


if __name__ == "__main__":
    unittest.main()
