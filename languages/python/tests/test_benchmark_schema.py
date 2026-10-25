"""The Prisma benchmark setup also works before its output directory exists."""

import shutil
import subprocess
from pathlib import Path


def test_prisma_schema_creates_output_directory(tmp_path):
    source = Path(__file__).resolve().parents[3] / "bench" / "compare"
    destination = tmp_path / "compare"
    (destination / "js").mkdir(parents=True)
    shutil.copy(source / "schema.prisma", destination / "schema.prisma")
    script = destination / "js" / "make-prisma-schema.mjs"
    shutil.copy(source / "js" / script.name, script)
    for _ in range(2):
        subprocess.run(["node", str(script)], check=True, capture_output=True, text=True)
        schema = (destination / "js" / "prisma" / "schema.prisma").read_text()
        assert 'provider = "prisma-client"' in schema
        assert "model Post" in schema
        assert "@relation(through:" not in schema
