"""Run scripts/check-extension-names.py on a fake tree and read its exit code and output."""

from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "check-extension-names.py"


def run(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run([sys.executable, str(SCRIPT), str(root)], capture_output=True, text=True)


class CheckExtensionNamesTest(unittest.TestCase):
    def tree(self, files: dict[str, str]) -> Path:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        base = {
            "extensions/x/orm-extension.toml": 'id = "acme.x"\nlibrary = "acme_x"\n[[attributes]]\nname = "x.tag"\n',
            "crates/core/src/lib.rs": "fn main() {}\n",
        }
        for name, text in {**base, **files}.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return root

    def test_host_source_naming_an_extension_fails_with_file_and_line(self) -> None:
        root = self.tree({"crates/core/src/lib.rs": 'fn a() {}\nconst ID: &str = "acme.x";\n'})
        result = run(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("crates/core/src/lib.rs:2:", result.stdout)

    def test_same_name_in_test_data_passes(self) -> None:
        root = self.tree({
            "crates/core/src/tests/case.rs": 'const ID: &str = "acme.x";\n',
            "crates/core/src/tests.rs": 'const ID: &str = "x.tag";\n',
            "crates/core/src/lib.rs": 'fn a() {}\n#[cfg(test)]\nmod t { const ID: &str = "acme.x"; }\n',
        })
        result = run(root)
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_feature_name_in_cargo_features_fails(self) -> None:
        root = self.tree({"crates/core/Cargo.toml": '[features]\nx-more = []\n'})
        self.assertEqual(run(root).returncode, 0)
        root = self.tree({"crates/core/Cargo.toml": '[package]\nname = "x"\n[features]\nx-more = []\nx = []\n'})
        result = run(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("crates/core/Cargo.toml:5:", result.stdout)

    def test_derived_names_and_outside_manifest_are_checked(self) -> None:
        outside = 'id = "acme.out-side"\nlibrary = "out_lib"\n'
        for name in ("acme-out-side", "acme_out_side", "@acme/out-side", "out_lib"):
            root = self.tree({
                "tests/outside-extension/orm-extension.toml": outside,
                "crates/engine/src/lib.rs": f'const N: &str = "{name}";\n',
            })
            self.assertEqual(run(root).returncode, 1, name)

    def test_lone_cfg_test_item_does_not_hide_later_lines(self) -> None:
        root = self.tree({"crates/core/src/lib.rs": '#[cfg(test)]\nfn t() {}\nconst ID: &str = "acme.x";\n'})
        self.assertEqual(run(root).returncode, 1)

    def test_bare_word_is_not_a_name(self) -> None:
        root = self.tree({"crates/core/src/lib.rs": "// acme and x are words\n"})
        self.assertEqual(run(root).returncode, 0)


if __name__ == "__main__":
    unittest.main()
