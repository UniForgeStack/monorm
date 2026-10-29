"""Run scripts/check-glibc.py with a fake `objdump` that prints a chosen symbol table."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "check-glibc.py"


class CheckGlibcTest(unittest.TestCase):
    def run_script(self, symbols: str, wheel: bool = False, member: str = "pkg/_native.so") -> subprocess.CompletedProcess[str]:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        fake = root / "objdump"
        fake.write_text(f"#!/bin/sh\ncat <<'EOT'\n{symbols}\nEOT\n")
        fake.chmod(0o755)
        library = root / "libx.so"
        library.write_bytes(b"")
        target = library
        if wheel:
            target = root / "x.whl"
            with zipfile.ZipFile(target, "w") as archive:
                archive.write(library, member)
        env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}"}
        return subprocess.run([sys.executable, str(SCRIPT), str(target)], capture_output=True, text=True, env=env)

    def test_floor_passes(self):
        result = self.run_script("0 DF *UND* 0 (GLIBC_2.17) memcpy\n0 DF *UND* 0 (GLIBC_2.28) fcntl64")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_newer_symbol_fails(self):
        result = self.run_script("0 DF *UND* 0 (GLIBC_2.28) a\n0 DF *UND* 0 (GLIBC_2.39) b")
        self.assertEqual(result.returncode, 1)
        self.assertIn("needs glibc 2.39", result.stdout)

    def test_reads_libraries_inside_a_wheel(self):
        result = self.run_script("0 DF *UND* 0 (GLIBC_2.34) b", wheel=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("_native.so", result.stdout)

    def test_a_wheel_without_a_library_fails(self):
        result = self.run_script("0 DF *UND* 0 (GLIBC_2.17) a", wheel=True, member="pkg/__init__.py")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no library", result.stdout)


if __name__ == "__main__":
    unittest.main()
