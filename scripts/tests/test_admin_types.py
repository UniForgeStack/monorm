"""The committed admin config and UI types equal what scripts/gen-admin-types.py renders from the Rust structs."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PYTHON = ROOT / "languages/python/monorm/_admin_types.py"
TYPESCRIPT = ROOT / "languages/js/src/admin-types.ts"
API_TYPES = ROOT / "crates/admin/ui/src/generated/api-types.ts"
REGENERATE = "python3 scripts/gen-admin-types.py"


class AdminTypesTest(unittest.TestCase):
    def test_the_committed_types_are_current(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            python, typescript, api = Path(tmp, "_admin_types.py"), Path(tmp, "admin-types.ts"), Path(tmp, "api-types.ts")
            run = subprocess.run(
                [sys.executable, str(ROOT / "scripts/gen-admin-types.py"), "--python", str(python), "--typescript", str(typescript), "--api-types", str(api)],
                cwd=ROOT, capture_output=True, text=True,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(python.read_text(), PYTHON.read_text(), f"{PYTHON.name} is stale: run `{REGENERATE}`")
            self.assertEqual(typescript.read_text(), TYPESCRIPT.read_text(), f"{TYPESCRIPT.name} is stale: run `{REGENERATE}`")
            self.assertEqual(api.read_text(), API_TYPES.read_text(), f"{API_TYPES.name} is stale: run `{REGENERATE}`")

    def test_a_schema_file_gives_the_same_output(self) -> None:
        run = subprocess.run(["cargo", "run", "-q", "-p", "monorm-admin", "--bin", "admin-schema"], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        ui = subprocess.run(["cargo", "run", "-q", "-p", "monorm-admin", "--bin", "admin-schema", "--", "ui"], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(ui.returncode, 0, ui.stderr)
        with tempfile.TemporaryDirectory() as tmp:
            schema, python, typescript = Path(tmp, "schema.json"), Path(tmp, "_admin_types.py"), Path(tmp, "admin-types.ts")
            ui_schema, api = Path(tmp, "ui-schema.json"), Path(tmp, "api-types.ts")
            schema.write_text(run.stdout)
            ui_schema.write_text(ui.stdout)
            gen = subprocess.run(
                [
                    sys.executable, str(ROOT / "scripts/gen-admin-types.py"), "--schema", str(schema), "--python", str(python), "--typescript", str(typescript),
                    "--ui-schema", str(ui_schema), "--api-types", str(api),
                ],
                cwd=ROOT, capture_output=True, text=True,
            )
            self.assertEqual(gen.returncode, 0, gen.stderr)
            self.assertEqual(python.read_text(), PYTHON.read_text())
            self.assertEqual(typescript.read_text(), TYPESCRIPT.read_text())
            self.assertEqual(api.read_text(), API_TYPES.read_text())

    def test_no_schema_attribute_renames_a_wire_key(self) -> None:
        # The generated key must be the key that serde reads: a `schemars` rename would split the two.
        for name in ("site_config.rs", "wire.rs"):
            self.assertNotIn("schemars(rename", (ROOT / "crates/admin/src" / name).read_text(), name)

if __name__ == "__main__":
    unittest.main()
