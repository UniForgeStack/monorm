"""Exercise the public package command with a crate outside the workspace."""
import base64
import hashlib
import json
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def archive_metadata(wheel):
    with zipfile.ZipFile(wheel) as archive:
        return archive.read(next(n for n in archive.namelist() if n.endswith(".dist-info/METADATA"))).decode()


class PackageTest(unittest.TestCase):
    def test_outside_package_is_installable_and_carries_the_manifest_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run([
                "cargo", "run", "-q", "-p", "monorm-extension-package", "--", "package",
                str(ROOT / "tests/outside-extension"), "--out", tmp,
                "--tsc", str(ROOT / "languages/js/node_modules/.bin/tsc"),
            ], cwd=ROOT, check=True)
            wheel, = Path(tmp).glob("*.whl")
            self.assertIn("-py3-none-", wheel.name)
            with zipfile.ZipFile(wheel) as archive:
                manifest = archive.read("acme_outside/monorm-extension.toml")
                digest = hashlib.sha256(manifest).hexdigest()
                self.assertIn(digest, archive.read("acme_outside/__init__.py").decode())
                self.assertIn('acme.outside = acme_outside', archive.read("acme_outside-0.1.0.dist-info/entry_points.txt").decode())
                self.assertTrue(any("outside_extension." in name for name in archive.namelist()))
                self.assertIn("Requires-Dist: typing-extensions>=4.0\n", archive.read("acme_outside-0.1.0.dist-info/METADATA").decode())
                for line in archive.read("acme_outside-0.1.0.dist-info/RECORD").decode().splitlines():
                    name, hashed, size = line.split(",")
                    if hashed:
                        data = archive.read(name)
                        self.assertEqual(hashed, "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode())
                        self.assertEqual(int(size), len(data))
            npm = Path(tmp, "npm")
            package_json = json.loads((npm / "package.json").read_text())
            self.assertEqual(package_json["monorm"]["extension"], "acme.outside")
            self.assertEqual(package_json["dependencies"], {"decimal.js": "^10.4.3"})
            self.assertEqual(package_json["exports"]["./model"], {"types": "./model.d.ts", "default": "./model.js"})
            self.assertEqual(package_json["exports"]["."]["default"], "./index.js")
            self.assertTrue((npm / "model.js").is_file())
            self.assertEqual((npm / "monorm-extension.toml").read_bytes(), manifest)
            self.assertIn(digest, (npm / "index.js").read_text())
            self.assertTrue((npm / "index.d.ts").is_file())
            self.assertNotIn("license", package_json)
            self.assertNotIn("License: MIT", archive_metadata(wheel))

    def test_own_extension_carries_the_publish_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run([
                "cargo", "run", "-q", "-p", "monorm-extension-package", "--", "package",
                str(ROOT / "extensions/timestamps"), "--out", tmp,
                "--tsc", str(ROOT / "languages/js/node_modules/.bin/tsc"),
            ], cwd=ROOT, check=True)
            wheel, = Path(tmp).glob("*.whl")
            metadata = archive_metadata(wheel)
            for line in ("License: MIT", "Project-URL: Repository, https://github.com/UniForgeStack/monorm", "Description-Content-Type: text/markdown"):
                self.assertIn(line, metadata)
            package_json = json.loads((Path(tmp, "npm") / "package.json").read_text())
            self.assertEqual(package_json["name"], "monorm-timestamps")
            self.assertEqual(package_json["license"], "MIT")
            self.assertEqual(package_json["publishConfig"], {"access": "public"})
            self.assertEqual(package_json["repository"]["directory"], "extensions/timestamps")
            self.assertIn("README.md", package_json["files"])


if __name__ == "__main__":
    unittest.main()
