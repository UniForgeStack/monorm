import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RELEASE = ROOT / "scripts" / "release"
sys.path.insert(0, str(RELEASE))
import versions  # noqa: E402

spec = importlib.util.spec_from_file_location("publish_lists", RELEASE / "publish-lists.py")
publish_lists = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publish_lists)

CHANGELOG = "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - TBD\n"


def run(script: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(RELEASE / script), *args], capture_output=True, text=True)


def copy_sites(root: Path):
    paths = [path for pattern, *_ in versions.TEXT_SITES for path in ROOT.glob(pattern)]
    for path in [*paths, *(ROOT / name for name in versions.JSON_SITES)]:
        target = root / path.relative_to(ROOT)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)


def snapshot(root: Path) -> dict[Path, bytes]:
    return {p: p.read_bytes() for p in root.rglob("*") if p.is_file()}


class VersionsTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        copy_sites(self.root)
        (self.root / "CHANGELOG.md").write_text(CHANGELOG)
        # The copy is pinned, so these tests hold on a tree at any version, such as the rc branch.
        self.set("0.1.0")

    def set(self, version: str):
        result = run("versions.py", "set", version, "--root", str(self.root))
        self.assertEqual(result.returncode, 0, result.stderr)

    def check(self, *args: str):
        return run("versions.py", "check", *args, "--root", str(self.root))

    def test_the_real_tree_agrees_with_its_own_version(self):
        result = run("versions.py", "check", "--allow-tbd")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_set_to_the_tree_version_changes_no_byte_of_the_real_files(self):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root)
        copy_sites(root)
        before = snapshot(root)
        self.assertEqual(run("versions.py", "set", versions.tree_version(root), "--root", str(root)).returncode, 0)
        self.assertEqual(before, snapshot(root))

    def test_a_tbd_date_passes_only_with_allow_tbd(self):
        self.assertEqual(self.check("0.1.0").returncode, 1)
        self.assertEqual(self.check("0.1.0", "--allow-tbd").returncode, 0)

    def test_a_missing_changelog_heading_fails(self):
        result = self.check("0.1.1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("pyproject.toml: 0.1.0 (want 0.1.1)", result.stdout)
        self.assertIn("CHANGELOG.md: no heading ## [0.1.1]", result.stdout)

    def test_one_wrong_site_fails_and_is_named(self):
        path = self.root / "extensions/inherit/monorm-extension.toml"
        path.write_text(path.read_text().replace('version = "0.1.0"', 'version = "0.1.1"', 1))
        result = self.check("0.1.0", "--allow-tbd")
        self.assertEqual(result.returncode, 1)
        self.assertIn("extensions/inherit/monorm-extension.toml: 0.1.1", result.stdout)

    def test_set_writes_each_form_and_set_back_restores_every_file(self):
        before = snapshot(self.root)
        self.set("0.1.0rc1")
        self.assertEqual(self.check("0.1.0-rc.1").returncode, 0, "a release candidate needs no changelog heading")
        self.assertIn('version = "0.1.0rc1"', (self.root / "pyproject.toml").read_text())
        self.assertIn('"monorm-native-postgres==0.1.0rc1"', (self.root / "pyproject.toml").read_text())
        self.assertIn('version = "0.1.0-rc.1"', (self.root / "extensions/inherit/monorm-extension.toml").read_text())
        self.assertIn("0.1.0-rc.1", (self.root / "languages/js/src/native.ts").read_text())
        package = json.loads((self.root / "languages/js/package.json").read_text())
        self.assertEqual(package["peerDependencies"]["@ufstack/monorm-native-sqlite"], "0.1.0-rc.1")
        lock = json.loads((self.root / "languages/js/package-lock.json").read_text())
        self.assertEqual(lock["packages"][""]["version"], "0.1.0-rc.1")
        self.set("0.1.0")
        self.assertEqual(before, snapshot(self.root))

    def test_set_writes_the_cargo_lock_entries_and_the_lock_links(self):
        self.set("0.1.0rc1")
        cargo = (self.root / "Cargo.lock").read_text()
        for crate in ("monorm-contracts", "monorm-node", "monorm-python"):
            self.assertIn(f'name = "{crate}"\nversion = "0.1.0-rc.1"', cargo)
        self.assertIn('name = "monorm-contracts"\nversion = "0.1.0-rc.1"', (self.root / "tests/outside-extension/Cargo.lock").read_text())
        js = json.loads((self.root / "languages/js/package-lock.json").read_text())["packages"]
        self.assertEqual(js["../../target/extension-packages/inherit/npm"]["version"], "0.1.0-rc.1")
        auth = json.loads((self.root / "apps/auth/typescript/package-lock.json").read_text())["packages"]
        self.assertEqual(auth["../../../languages/js"]["version"], "0.1.0-rc.1")
        self.assertEqual(auth["../../../languages/js"]["peerDependencies"]["@ufstack/monorm-native-tooling"], "0.1.0-rc.1")
        self.assertEqual(self.check("0.1.0-rc.1").returncode, 0)

    def test_a_stale_cargo_lock_entry_fails_and_is_named(self):
        path = self.root / "Cargo.lock"
        path.write_text(path.read_text().replace('name = "monorm-node"\nversion = "0.1.0"', 'name = "monorm-node"\nversion = "0.1.1"'))
        result = self.check("0.1.0", "--allow-tbd")
        self.assertEqual(result.returncode, 1)
        self.assertIn("Cargo.lock: 0.1.1", result.stdout)

    def test_a_stale_lock_link_entry_fails_and_is_named(self):
        path = self.root / "languages/js/package-lock.json"
        data = json.loads(path.read_text())
        data["packages"]["../../target/extension-packages/inherit/npm"]["version"] = "0.1.1"
        path.write_text(json.dumps(data, indent=2) + "\n")
        result = self.check("0.1.0", "--allow-tbd")
        self.assertEqual(result.returncode, 1)
        self.assertIn("languages/js/package-lock.json: 0.1.1", result.stdout)

    def test_check_without_a_version_takes_the_one_in_pyproject(self):
        self.set("0.1.0rc1")
        self.assertEqual(self.check().returncode, 0)
        path = self.root / "extensions/inherit/monorm-extension.toml"
        path.write_text(path.read_text().replace('version = "0.1.0-rc.1"', 'version = "0.1.0"', 1))
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("extensions/inherit/monorm-extension.toml: 0.1.0 (want 0.1.0-rc.1)", result.stdout)

    def test_show_prints_the_semver_form_of_the_tree_version(self):
        self.assertEqual(run("versions.py", "show", "--root", str(self.root)).stdout, "0.1.0\n")
        self.set("0.1.0rc1")
        self.assertEqual(run("versions.py", "show", "--root", str(self.root)).stdout, "0.1.0-rc.1\n")

    def test_tag_accepts_the_semver_forms_only(self):
        for tag, version in (("v0.1.0", "0.1.0"), ("v0.1.0-rc.1", "0.1.0-rc.1")):
            result = run("versions.py", "tag", tag)
            self.assertEqual((result.returncode, result.stdout), (0, f"{version}\n"))
        for tag in ("v0.1.0rc1", "0.1.0", "v1.0", "v0.1.0-beta"):
            result = run("versions.py", "tag", tag)
            self.assertEqual(result.returncode, 1, tag)
            self.assertIn("use v0.1.0 or v0.1.0-rc.1", result.stderr)

    def test_a_bad_version_is_an_error(self):
        self.assertNotEqual(run("versions.py", "set", "1.0", "--root", str(self.root)).returncode, 0)


def touch_wheel(directory: Path, name: str, version: str, tag: str = "py3-none-any"):
    (directory / f"{name}-{version}-{tag}.whl").write_bytes(b"")


def pypi_dir(version: str = "0.1.0") -> Path:
    directory = Path(tempfile.mkdtemp())
    for project, kinds in publish_lists.PYPI.items():
        module = project.replace("-", "_")
        for index, kind in enumerate(kinds):
            if kind == "sdist":
                (directory / f"{module}-{version}.tar.gz").write_bytes(b"")
            else:
                touch_wheel(directory, module, version, f"cp3{index}-cp3{index}-linux_x86_64" if "native" in project else "py3-none-any")
    return directory


def npm_dir(version: str = "0.1.0") -> Path:
    directory = Path(tempfile.mkdtemp())
    for index, name in enumerate(publish_lists.NPM):
        manifest = directory / "package.json"
        manifest.write_text(json.dumps({"name": name, "version": version}))
        with tarfile.open(directory / f"p{index}.tgz", "w:gz") as tar:
            tar.add(manifest, arcname="package/package.json")
        manifest.unlink()
    return directory


class PublishListsTest(unittest.TestCase):
    def test_the_lists_have_15_pypi_projects_and_27_npm_names(self):
        self.assertEqual(len(publish_lists.PYPI), 15)
        self.assertEqual(len(publish_lists.NPM), 27)

    def test_a_complete_pypi_directory_passes(self):
        directory = pypi_dir()
        self.addCleanup(shutil.rmtree, directory)
        result = run("publish-lists.py", "check", "pypi", str(directory), "0.1.0")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_a_missing_wheel_and_a_wrong_version_fail(self):
        directory = pypi_dir()
        self.addCleanup(shutil.rmtree, directory)
        next(directory.glob("monorm_locking-*")).unlink()
        touch_wheel(directory, "monorm_proxy", "0.2.0", "py3-none-any")
        result = run("publish-lists.py", "check", "pypi", str(directory), "0.1.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("monorm-locking: files []", result.stdout)
        self.assertIn("monorm_proxy-0.2.0", result.stdout)

    def test_the_rc_version_uses_the_pep_440_form_for_pypi_and_semver_for_npm(self):
        pypi, npm = pypi_dir("0.1.0rc1"), npm_dir("0.1.0-rc.1")
        self.addCleanup(shutil.rmtree, pypi)
        self.addCleanup(shutil.rmtree, npm)
        self.assertEqual(run("publish-lists.py", "check", "pypi", str(pypi), "0.1.0-rc.1").returncode, 0)
        self.assertEqual(run("publish-lists.py", "check", "npm", str(npm), "0.1.0rc1").returncode, 0)

    def test_npm_rejects_an_extra_package_and_a_duplicate(self):
        directory = npm_dir()
        self.addCleanup(shutil.rmtree, directory)
        shutil.copy(directory / "p0.tgz", directory / "copy.tgz")
        extra = directory / "package.json"
        extra.write_text(json.dumps({"name": "decimal.js", "version": "0.1.0"}))
        with tarfile.open(directory / "decimal.tgz", "w:gz") as tar:
            tar.add(extra, arcname="package/package.json")
        result = run("publish-lists.py", "check", "npm", str(directory), "0.1.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("decimal.js: not a package of this release", result.stdout)
        self.assertIn("2 files, want 1", result.stdout)

    def test_split_puts_only_the_thin_monorm_files_apart(self):
        directory, out = pypi_dir(), Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, directory)
        self.addCleanup(shutil.rmtree, out)
        self.assertEqual(run("publish-lists.py", "split-pypi", str(directory), str(out)).returncode, 0)
        self.assertEqual(sorted(p.name for p in (out / "monorm").iterdir()), ["monorm-0.1.0-py3-none-any.whl", "monorm-0.1.0.tar.gz"])
        self.assertEqual(len(list((out / "deps").iterdir())), len(list(directory.iterdir())) - 2)


FAKE_NPM = """#!/bin/sh
echo "$@" >> "$FAKE_NPM_LOG"
if [ "$1" = view ]; then
  case "$2" in "$FAKE_NPM_UP"*) echo "${2##*@}"; exit 0;; esac
  exit 1
fi
exit "${FAKE_NPM_FAIL:-0}"
"""


@unittest.skipUnless(shutil.which("node") and shutil.which("tar"), "needs node and tar")
class NpmPublishTest(unittest.TestCase):
    def setUp(self):
        self.dir = npm_dir()
        self.tools = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.addCleanup(shutil.rmtree, self.tools)
        self.fake = self.tools / "npm"
        self.fake.write_text(FAKE_NPM)
        self.fake.chmod(0o755)
        self.log = self.tools / "log"

    def publish(self, mode: str, **env: str):
        return subprocess.run(
            ["node", str(RELEASE / "npm-publish.mjs"), str(self.dir), "--mode", mode],
            capture_output=True, text=True,
            env={**os.environ, "NPM_BIN": str(self.fake), "FAKE_NPM_LOG": str(self.log), "FAKE_NPM_UP": "none", **env},
        )

    def published(self) -> list[str]:
        names = []
        for line in self.log.read_text().splitlines():
            if line.startswith("publish "):
                with tarfile.open(line.split()[1]) as tar:
                    names.append(json.load(tar.extractfile("package/package.json"))["name"])
        return names

    def test_publishes_in_dependency_order_with_provenance(self):
        result = self.publish("real")
        self.assertEqual(result.returncode, 0, result.stderr)
        names = self.published()
        self.assertEqual(len(names), 27)
        position = {name: i for i, name in enumerate(names)}
        for profile in publish_lists.PROFILES:
            for platform in publish_lists.PLATFORMS:
                self.assertLess(position[f"@ufstack/monorm-native-{profile}-{platform}"], position[f"@ufstack/monorm-native-{profile}"])
            self.assertLess(position[f"@ufstack/monorm-native-{profile}"], position["@ufstack/monorm"])
        self.assertLess(position["@ufstack/monorm"], position["@ufstack/monorm-composition"])
        self.assertEqual(names[-1], "@ufstack/monorm-auth")
        self.assertIn("--provenance --access public", self.log.read_text())

    def test_dry_run_never_passes_provenance_and_always_dry_runs(self):
        self.assertEqual(self.publish("dry-run").returncode, 0)
        publishes = [l for l in self.log.read_text().splitlines() if l.startswith("publish ")]
        self.assertEqual(len(publishes), 27)
        self.assertTrue(all("--dry-run" in l and "--provenance" not in l for l in publishes))

    def test_a_prerelease_gets_the_next_tag(self):
        self.dir = npm_dir("0.1.0-rc.1")
        self.addCleanup(shutil.rmtree, self.dir)
        self.assertEqual(self.publish("dry-run").returncode, 0)
        publishes = [l for l in self.log.read_text().splitlines() if l.startswith("publish ")]
        self.assertTrue(all("--tag next" in l for l in publishes))

    def test_a_release_gets_no_tag(self):
        self.assertEqual(self.publish("dry-run").returncode, 0)
        self.assertNotIn("--tag", self.log.read_text())

    def test_a_version_that_is_up_is_skipped(self):
        result = self.publish("real", FAKE_NPM_UP="@ufstack/monorm-native-sqlite")
        self.assertEqual(result.returncode, 0, result.stderr)
        # The fake matches by prefix, so the profile and its 3 platform packages are all up.
        self.assertEqual(len(self.published()), 27 - 4)
        self.assertIn("skip @ufstack/monorm-native-sqlite@0.1.0", result.stdout)

    def test_a_failed_publish_stops_the_run(self):
        result = self.publish("real", FAKE_NPM_FAIL="1")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(len(self.published()), 1)


try:
    import yaml
except ImportError:
    yaml = None


@unittest.skipUnless(yaml, "needs PyYAML")
class ReleaseWorkflowTest(unittest.TestCase):
    def setUp(self):
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())["jobs"]

    def test_only_the_three_publish_jobs_hold_id_token(self):
        publish = {"publish-crates", "publish-pypi", "publish-npm"}
        holders = {n for n, j in self.jobs.items() if "id-token" in j.get("permissions", {})}
        self.assertEqual(holders, publish)
        for name in publish:
            self.assertEqual(self.jobs[name]["environment"], "release")
            self.assertEqual(self.jobs[name]["permissions"]["id-token"], "write")

    def test_the_build_workflows_are_reusable_and_a_tag_does_not_start_them(self):
        for name in ("package-profiles.yml", "extension-packages.yml"):
            triggers = yaml.safe_load((ROOT / ".github/workflows" / name).read_text())[True]
            self.assertIn("workflow_call", triggers)
            self.assertNotIn("tags", triggers.get("push") or {})

    def test_a_publish_job_waits_for_the_collected_files(self):
        for name in ("publish-crates", "publish-pypi", "publish-npm"):
            self.assertIn("collect", self.jobs[name]["needs"])

    def run_text(self, job, **key):
        (field, value), = key.items()
        return next(s["run"] for s in self.jobs[job]["steps"] if s.get(field) == value)

    def test_the_version_step_reads_the_tag_and_the_tree_through_versions_py(self):
        text = self.run_text("version", id="version")
        self.assertIn("versions.py tag", text)
        self.assertIn("versions.py show", text)

    def test_the_collect_job_builds_the_thin_monorm_files_itself(self):
        text = self.run_text("collect", name="Gather the PyPI files")
        self.assertIn("rm -f dist/pypi/monorm-*.whl", text)
        self.assertIn("uv build --out-dir dist/pypi .", text)


if __name__ == "__main__":
    unittest.main()
