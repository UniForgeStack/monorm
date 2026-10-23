"""Load a schema with the generated language members of the soft-delete and locking extensions, as a generated models module does."""

import importlib
import json
import sys
import tempfile
from pathlib import Path
from types import ModuleType
from typing import Any

import orm
from orm import _native
from orm.model import define

ID = "orm.soft-delete"
EXTENSIONS = {"orm.soft-delete": "orm_soft_delete", "orm.locking": "orm_locking"}
_modules: dict[str, ModuleType] | None = None


def extension_modules() -> dict[str, ModuleType]:
    """The `orm_ext` packages that `orm generate` writes for the loaded artifact, made once for the session."""
    global _modules
    if _modules is None:
        root = Path(tempfile.mkdtemp(prefix="orm-extensions-"))
        (root / "schema.prisma").write_text("model Probe {\n  id Int @id\n}\n")
        code = _native.cli(
            ["--schema", str(root / "schema.prisma"), "generate", "python", "-o", str(root / "models.py")]
        )
        assert code == 0, "orm generate failed"
        sys.path.insert(0, str(root))
        _modules = {}
        for extension, package in EXTENSIONS.items():
            if (root / "orm_ext" / package).is_dir():
                _modules[extension] = importlib.import_module(f"orm_ext.{package}")
    return _modules


def soft_delete_module() -> ModuleType:
    return extension_modules()[ID]


def loads_soft_delete(source: str, *, registry: orm.Registry | None = None) -> dict[str, Any]:
    """`orm.loads`, with the members of each generated extension on each model it has a record for, a proxy too."""
    ir = _native.compile_schema(source)
    behavior = json.loads(ir).get("behavior", {})
    modules = extension_modules()
    members: dict[str, list[str]] = {}
    for extension in modules:
        prefix = extension.split(".")[1].replace("-", "_") + "."
        names = {d["model"] for d in behavior.get("declarations", ()) if d["attribute"].startswith(prefix)}
        for rows in (v for v in behavior.values() if isinstance(v, list)):
            names |= {r["model"] for r in rows if isinstance(r, dict) and r.get("source") == extension and "model" in r}
        for name in names:
            members.setdefault(name, []).append(extension)
    return define(
        ir,
        registry=registry,
        extensions=dict(modules),
        members={name: tuple(sorted(ids, key=list(modules).index)) for name, ids in sorted(members.items())},
    )
