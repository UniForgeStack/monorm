"""The SQL a migration would hold, without writing it: for tests that check generated statements."""

from __future__ import annotations

import json
import shutil
import tempfile
from pathlib import Path

from monorm.migrations import Migrations


def _statements(text: str) -> list[str]:
    out = []
    for chunk in text.split("\n\n"):
        lines = [line for line in chunk.splitlines() if not line.startswith("--")]
        sql = "\n".join(lines).strip().removesuffix(";")
        if sql:
            out.append(sql)
    return out


def planned(migrations: Migrations, name: str | None = None) -> tuple[list[str], list[str]]:
    """The up and down statements of the migration `migrations.make()` would write, in order."""
    with tempfile.TemporaryDirectory() as scratch:
        if Path(migrations.directory).is_dir():
            shutil.copytree(migrations.directory, scratch, dirs_exist_ok=True)
        made = Migrations(scratch, migrations._schema, online=migrations.online, name_template=migrations.name_template).make(name)
        if made is None:
            return [], []
        return _statements(made.up_sql), _statements(made.down_sql)


def planned_up(migrations: Migrations, name: str | None = None) -> list[str]:
    return planned(migrations, name)[0]


def append_op(migration, op: dict) -> None:
    """Adds an operation to the end of a written migration file."""
    data = json.loads(migration.path.read_text())
    data["operations"].append(op)
    migration.path.write_text(json.dumps(data, indent=2) + "\n")


def write_migration(directory, name: str, operations: list[dict], **top) -> Path:
    """Writes a hand-made migration file `<name>.json` and returns its path."""
    path = Path(directory) / f"{name}.json"
    path.write_text(json.dumps({"version": 1, **top, "operations": operations}, indent=2) + "\n")
    return path
