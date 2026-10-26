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
                "cargo", "run", "-q", "-p", "orm-extension-package", "--", "package",
                str(ROOT / "tests/outside-extension"), "--out", tmp,
                "--tsc", str(ROOT / "languages/js/node_modules/.bin/tsc"),
            ], cwd=ROOT, check=True)
            wheel, = Path(tmp).glob("*.whl")
            self.assertIn("-py3-none-", wheel.name)
            with zipfile.ZipFile(wheel) as archive:
                manifest = archive.read("acme_outside/orm-extension.toml")
                digest = hashlib.sha256(manifest).hexdigest()
                self.assertIn(digest, archive.read("acme_outside/__init__.py").decode())
                self.assertIn('acme.outside = acme_outside', archive.read("acme_outside-0.1.0.dist-info/entry_points.txt").decode())
                self.assertTrue(any("outside_extension." in name for name in archive.namelist()))
                for line in archive.read("acme_outside-0.1.0.dist-info/RECORD").decode().splitlines():
                    name, hashed, size = line.split(",")
                    if hashed:
                        data = archive.read(name)
                        self.assertEqual(hashed, "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode())
                        self.assertEqual(int(size), len(data))
            npm = Path(tmp, "npm")
            self.assertEqual(json.loads((npm / "package.json").read_text())["orm"]["extension"], "acme.outside")
            self.assertEqual((npm / "orm-extension.toml").read_bytes(), manifest)
            self.assertIn(digest, (npm / "index.js").read_text())
            self.assertTrue((npm / "index.d.ts").is_file())


if __name__ == "__main__":
    unittest.main()
