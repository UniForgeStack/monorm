"""Load a schema with the generated soft-delete extension members, as a generated models module does."""

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
_module: ModuleType | None = None


def soft_delete_module() -> ModuleType:
    """The `orm_ext` package that `orm generate` writes for the loaded artifact, made once for the session."""
    global _module
    if _module is None:
        root = Path(tempfile.mkdtemp(prefix="orm-soft-delete-"))
        (root / "schema.prisma").write_text("model Probe {\n  id Int @id\n}\n")
        code = _native.cli(
            ["--schema", str(root / "schema.prisma"), "generate", "python", "-o", str(root / "models.py")]
        )
        assert code == 0, "orm generate failed"
        sys.path.insert(0, str(root))
        _module = importlib.import_module("orm_ext.orm_soft_delete")
    return _module


def loads_soft_delete(source: str, *, registry: orm.Registry | None = None) -> dict[str, Any]:
    """`orm.loads`, with the soft-delete members on each model the extension has a record for, a proxy too."""
    ir = _native.compile_schema(source)
    behavior = json.loads(ir).get("behavior", {})
    models = {d["model"] for d in behavior.get("declarations", ()) if d["attribute"].startswith("soft_delete.")}
    for kind in ("read_defaults", "delete_rewrites", "write_assignments", "instance_guards"):
        models |= {r["model"] for r in behavior.get(kind, ()) if r["source"] == ID}
    return define(
        ir,
        registry=registry,
        extensions={ID: soft_delete_module()},
        members={name: (ID,) for name in sorted(models)},
    )
