import json
import subprocess
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# Private on purpose: never published. Everything else under version control is published.
PRIVATE = {"monorm-blog-example", "monorm-quickstart-example", "bench-compare-js"}
FIXTURES = ("scripts/tests/",)


def tracked(name):
    listed = subprocess.run(["git", "ls-files", f"*{name}"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split()
    return [ROOT / path for path in listed if not path.startswith(FIXTURES) and (ROOT / path).name == name]


class PackageMetadata(unittest.TestCase):
    def test_npm_packages(self):
        paths = tracked("package.json")
        self.assertTrue(paths)
        for path in paths:
            data = json.loads(path.read_text())
            where = path.relative_to(ROOT).as_posix()
            if data["name"] in PRIVATE:
                self.assertTrue(data.get("private"), where)
                continue
            self.assertTrue(data["name"] == "monorm" or data["name"].startswith("@ufstack/monorm"), where)
            self.assertFalse(data.get("private"), where)
            self.assertEqual(data.get("license"), "MIT", where)
            self.assertTrue((data.get("repository") or {}).get("url"), where)
            self.assertEqual((data.get("publishConfig") or {}).get("access"), "public", where)
            self.assertTrue(data.get("description"), where)

    def test_python_projects(self):
        paths = tracked("pyproject.toml")
        self.assertTrue(paths)
        for path in paths:
            project = tomllib.loads(path.read_text())["project"]
            where = path.relative_to(ROOT).as_posix()
            self.assertTrue(project["name"].startswith("monorm"), where)
            for field in ("description", "readme", "license", "authors", "urls"):
                self.assertIn(field, project, f"{where}: {field}")
            self.assertTrue((path.parent / project["readme"]).is_file(), where)
            for pattern in project.get("license-files", []):
                self.assertTrue(list(path.parent.glob(pattern)), f"{where}: {pattern}")

    def test_cargo_contracts(self):
        package = tomllib.loads((ROOT / "crates/contracts/Cargo.toml").read_text())["package"]
        for field in ("description", "license", "repository", "readme", "keywords", "categories"):
            self.assertIn(field, package, field)


if __name__ == "__main__":
    unittest.main()
