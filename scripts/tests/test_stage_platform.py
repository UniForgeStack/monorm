"""The platform key of `languages/python/packaging/stage.py`: the host key, `--platform`, and the musl keys."""

import importlib.util
import shutil
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest import mock

PACKAGING = Path(__file__).resolve().parents[2] / "languages" / "python" / "packaging"


def load():
    spec = importlib.util.spec_from_file_location("stage", PACKAGING / "stage.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class StagePlatformTest(unittest.TestCase):
    def host(self, system: str, machine: str, libc: str):
        stage = load()
        with mock.patch.object(stage.platform, "system", return_value=system), \
                mock.patch.object(stage.platform, "machine", return_value=machine), \
                mock.patch.object(stage.platform, "libc_ver", return_value=(libc, "")):
            return stage.host()

    def test_glibc_linux_keeps_its_key(self):
        self.assertEqual(self.host("Linux", "x86_64", "glibc")[0], "linux-x64")
        self.assertEqual(self.host("Linux", "aarch64", "glibc")[0], "linux-arm64")

    def test_musl_linux_gets_the_musl_key(self):
        self.assertEqual(self.host("Linux", "x86_64", "")[0], "linux-x64-musl")
        self.assertEqual(self.host("Linux", "aarch64", "")[0], "linux-arm64-musl")

    def test_other_systems_ignore_the_libc(self):
        self.assertEqual(self.host("Darwin", "arm64", "")[0], "darwin-arm64")
        self.assertEqual(self.host("Windows", "AMD64", "")[0], "win32-x64")

    def test_platform_names_a_key_with_its_file_naming(self):
        stage = load()
        self.assertEqual(stage.platform_named("linux-x64-musl"), ("linux-x64-musl", "lib", ".so"))
        self.assertEqual(stage.platform_named("win32-x64"), ("win32-x64", "", ".dll"))
        with self.assertRaises(SystemExit) as raised:
            stage.platform_named("linux-riscv")
        self.assertIn("linux-x64-musl", str(raised.exception))

    def test_a_musl_library_is_staged_from_its_platform_directory(self):
        stage = load()
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        name = "timestamps"
        manifest = tomllib.loads((stage.ROOT / "extensions" / name / "monorm-extension.toml").read_text())
        key, prefix, suffix = stage.platform_named("linux-x64-musl")
        file = stage.library_file(manifest["library"], prefix, suffix)
        for platform in ("linux-x64", "linux-x64-musl"):
            (root / "libs" / name / platform).mkdir(parents=True)
            (root / "libs" / name / platform / file).write_text(platform)
        (root / "out").mkdir()
        with mock.patch.object(stage, "TARGET", root / "out"):
            module = stage.stage_extension(name, root / "libs", key, prefix, suffix)
        self.assertEqual((root / "out" / module / file).read_text(), "linux-x64-musl")


if __name__ == "__main__":
    unittest.main()
