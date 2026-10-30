"""``python -m monorm``: the ``monorm`` command line, the same Rust code as the standalone
``monorm`` binary and ``npx monorm`` (``crates/cli/``). See ``python -m monorm --help``.

    python -m monorm check
    python -m monorm generate [-o models.py] [--query-set Model=module:Class]  # models.py + .pyi
    python -m monorm makemigrations [name] [--empty] [--check]
    python -m monorm sqlmigrate <migration> [--down]
    python -m monorm migrate [target]                  # runs data.py steps through Migrator
    python -m monorm rollback [--steps N | --to <migration>|zero]
    python -m monorm showmigrations
    python -m monorm pull [-o schema.prisma] [--force]
    python -m monorm baseline
    python -m monorm drift
    python -m monorm init [dir]                        # a starter project; also `uvx monorm init`

The schema file and migrations directory come from ``--schema`` / ``--dir`` or from
``[tool.monorm]`` in ``pyproject.toml`` (``schema = "schema.prisma"``,
``migrations = "migrations"``). The database URL comes from ``--url`` or
``MONORM_DATABASE_URL``.
"""

from __future__ import annotations

import sys

from . import _native
from ._apps import resolve_apps
from ._packages import resolve_extensions, runtime_modules
from ._native import SchemaError


def main(argv: list[str] | None = None) -> int:
    """Runs the command line; gives the exit code."""
    sys.stdout.flush()
    sys.stderr.flush()
    if not hasattr(_native, "cli"):
        raise RuntimeError(
            "CLI is unavailable in this build of monorm; build it with the `profile-tooling` feature"
        )
    args = sys.argv[1:] if argv is None else list(argv)
    if args[:1] == ["init"]:  # no schema, apps or extensions exist before init
        return _native.cli(args)
    try:
        apps = resolve_apps(_native.cli_schema(args))
        extensions = resolve_extensions()
    except SchemaError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    for name, path in apps.items():
        args += ["--app", f"{name}={path}"]
    for ident, manifest in extensions.items():
        args += ["--extension", f"{ident}={manifest}"]
    for ident, module in runtime_modules().items():
        args += ["--extension-module", f"{ident}={module}"]
    found = _native.cli_migrate_args(args)
    if found is not None:
        schema, directory, url, target = found
        from .migrations import has_data_steps, migrate_command

        if url:
            import asyncio

            try:
                # data.py runs in Python, so this migrator applies the directory
                if has_data_steps(directory):
                    return asyncio.run(migrate_command(schema, directory, url, target))
            except Exception as e:  # the CLI reports errors, not tracebacks
                print(f"error: {e}", file=sys.stderr)
                return 1
    return _native.cli(args)


if __name__ == "__main__":
    sys.exit(main())
