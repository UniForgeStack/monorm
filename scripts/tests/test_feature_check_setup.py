"""Exercise feature-check with a clean PATH and no prebuilt JS package or addon."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class FeatureCheckSetupTest(unittest.TestCase):
    def test_clean_runner_prepares_tools_and_types_before_suites(self) -> None:
        script = Path(__file__).resolve().parents[1] / "feature-check.sh"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            shutil.copy(script, root / "scripts/feature-check.sh")
            installer = root / "scripts/install-extension-packages.sh"
            installer.write_text("#!/bin/sh\nexit 0\n")
            installer.chmod(0o755)
            for directory in ("languages/js", "extensions/file-storage/integration/typescript"):
                (root / directory).mkdir(parents=True)
            tools = root / "tools"
            tools.mkdir()
            work = root / "work"
            venv = work / "venv/bin"
            venv.mkdir(parents=True)

            def executable(path: Path, body: str) -> None:
                path.write_text("#!/bin/sh\nset -eu\n" + body)
                path.chmod(0o755)

            # Simulate installed dev dependencies without inheriting host tools.
            executable(tools / "uv", "exit 0\n")
            executable(venv / "python", f'''
case "$*" in
  *sysconfig*) exec "{sys.executable}" "$@" ;;
  *) exit 0 ;;
esac
''')
            for program in ("mypy", "pyright"):
                executable(venv / program, "exit 0\n")
            executable(tools / "node", "exit 0\n")
            executable(tools / "bun", "exit 0\n")
            (root / "apps/auth/typescript").mkdir(parents=True)
            package = root / "languages/python/packaging/custom/orm_native_custom"
            package.mkdir(parents=True)
            (package / "__init__.py").touch()
            executable(tools / "npm", '''
case "$*" in
  *build*) touch "$ORM_FEATURE_CHECK_DIR/js-built" ;;
  test) test -f "$ORM_FEATURE_CHECK_DIR/js-built" && test -f "${ORM_NATIVE:-missing}" ;;
esac
''')
            executable(tools / "cargo", '''
case "$*" in
  test*) command -v mypy && command -v pyright ;;
  run*)
    mkdir -p "$ORM_FEATURE_CHECK_DIR/file-storage/build" "$ORM_FEATURE_CHECK_DIR/target/debug"
    for library in lib_native.so lib_native.dylib liborm_node.so liborm_node.dylib; do
      touch "$ORM_FEATURE_CHECK_DIR/target/debug/$library"
    done ;;
esac
''')
            result = subprocess.run(
                ["/bin/bash", str(root / "scripts/feature-check.sh"), "file-storage"],
                env={
                    **os.environ,
                    "PATH": f"{tools}:/usr/bin:/bin",
                    "ORM_FEATURE_CHECK_DIR": str(work),
                    "ORM_TEST_DATABASE_URL": "postgres://unused/fixture",
                    "ORM_NATIVE": "",
                },
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            matrix = (work / "matrix.txt").read_text()
            self.assertTrue((work / "js-built").exists(), "orm declarations must be built")
            for expected in (
                "storage rust-extension PASS",
                "storage typescript-integration PASS",
                "file-storage build PASS",
            ):
                with self.subTest(expected=expected):
                    self.assertIn(expected, matrix)


if __name__ == "__main__":
    unittest.main()
