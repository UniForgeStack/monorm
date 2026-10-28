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


if __name__ == "__main__":
    unittest.main()
