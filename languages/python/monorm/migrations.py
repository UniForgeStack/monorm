"""Migrations: generated from the schema file, stored as JSON operations, applied in order.

A migrations directory holds one JSON file per migration::

    migrations/
        0001_initial.json   # {"version": 1, "operations": [...]}
        0002_backfill.json
        0002_backfill.py    # a script the file names in a `run_code` operation

Generating a migration (the Rust core: ``crates/core/src/migrate``) replays the operations of the
existing files to get the schema they reach, and diffs the schema against it, so it needs no
database. The files are plain JSON and can be edited before they are applied. Applying and
reverting (``crates/engine/src/migrate.rs``) records each migration with a checksum in the
``monorm_migrations`` table and refuses to continue if an applied file changed. Both are the same
Rust code the ``monorm`` command line and the TypeScript package run, so every tool writes and
applies the same files. A ``run_code`` operation names a Python or TypeScript function that only its
own host runs.

A live database under migrations: :func:`pull` writes its schema file, :meth:`Migrator.baseline`
marks the first migration as applied (it does not run it), and :meth:`Migrator.drift`
compares the database with the schema the migrations reach.

From the command line: ``python -m monorm makemigrations / migrate / rollback /
showmigrations / sqlmigrate / pull / baseline / drift`` (see ``python -m monorm --help``).
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import inspect
import json
import os
import sys
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import _native
from ._apps import resolve_apps
from ._packages import resolve_extensions
from .db import Database, _callbacks, _Callbacks, _current_tx
from .db import connect as monorm_connect
from ._native import MigrationError
from .model import Registry

__all__ = [
    "Drift",
    "Migration",
    "Migrations",
    "Migrator",
    "MigrationError",
    "Plan",
    "PlanOperation",
    "Pulled",
    "Status",
    "Step",
    "pull",
]


@dataclass(frozen=True)
class Step:
    summary: str
    sql: str
    warning: str | None = None
    destructive: bool = False  # the step can lose rows; `allow_destructive` lets it run on a table with rows
    transaction: bool = True  # False for a step PostgreSQL refuses in a transaction (CREATE INDEX CONCURRENTLY)


@dataclass(frozen=True)
class PlanOperation:
    summary: str
    warning: str | None = None
    destructive: bool = False  # the operation can lose rows; `allow_destructive` lets it run on a table with rows
    atomic: bool = True  # False for an operation that runs outside a transaction (CREATE INDEX CONCURRENTLY)


@dataclass(frozen=True)
class Plan:
    """What the next migration would contain."""

    name: str
    operations: list[PlanOperation]

    @property
    def warnings(self) -> list[str]:
        return [o.warning for o in self.operations if o.warning]

    def __bool__(self) -> bool:
        return bool(self.operations)


@dataclass(frozen=True)
class Migration:
    name: str
    path: Path  # the JSON file
    _schema: Any = field(default=None, repr=False, compare=False)

    @property
    def operations(self) -> list[dict[str, Any]]:
        """The operations of the file, as JSON objects."""
        return list(json.loads(self.path.read_text())["operations"])

    @property
    def up_sql(self) -> str:
        """The SQL of the operations, numbered, for the schema's dialect (what ``sqlmigrate`` prints)."""
        return self._render(False)

    @property
    def down_sql(self) -> str:
        return self._render(True)

    def _render(self, down: bool) -> str:
        if self._schema is None:
            raise MigrationError("rendering SQL needs the schema: use Migrations.get() or Migrations.sqlmigrate()")
        return str(self._schema.sqlmigrate(str(self.path.parent), self.name, down))

    @property
    def checksum(self) -> str:
        return _native.migration_checksum(str(self.path.parent), self.name)

    def __str__(self) -> str:
        return self.name


Schema = Registry | _native.Schema | str | os.PathLike[str]


def _native_schema(schema: Schema) -> _native.Schema:
    if isinstance(schema, _native.Schema):
        return schema
    if isinstance(schema, Registry):
        return schema.prepare()
    return _native.Schema(_native.compile_schema_file(os.fspath(schema), resolve_apps(schema), resolve_extensions()))


class Migrations:
    """A migrations directory and the schema it migrates to: a schema file path, a
    :class:`~monorm.Registry` (e.g. of models from ``monorm.load``), or a compiled schema.

    ``online=True`` writes PostgreSQL migrations in the online form (the ``migrations.online`` setting
    of the command line): an index on an existing table in an ``"atomic": false`` operation. It
    changes nothing on SQLite.
    ``name_template`` is the file name template (the ``migrations.name`` setting; default ``"{number}_{slug}"``)."""

    def __init__(
        self,
        directory: str | os.PathLike[str],
        schema: Schema,
        *,
        online: bool = False,
        name_template: str | None = None,
    ) -> None:
        self.directory = Path(directory)
        self._schema = schema
        self.online = online
        self.name_template = name_template

    @property
    def schema(self) -> _native.Schema:
        return _native_schema(self._schema)

    def all(self) -> list[Migration]:
        schema = self.schema
        return [Migration(name, Path(path), schema) for name, path in _native.list_migrations(str(self.directory))]

    def get(self, name: str) -> Migration:
        """A migration by file name or order key (``"3"``, ``"0003_add_tags"``)."""
        found, path = _native.find_migration(str(self.directory), name)
        return Migration(found, Path(path), self.schema)

    def plan(self, name: str | None = None) -> Plan:
        """What the next migration would contain (empty if the schema is unchanged)."""
        new_name, ops = self.schema.plan_migration(str(self.directory), self.name_template, self.online, name)
        return Plan(new_name, [PlanOperation(*o) for o in ops])

    def make(self, name: str | None = None, *, empty: bool = False) -> Migration | None:
        """Write the next migration; returns it, or None when there is nothing to migrate.

        ``empty=True`` writes one even without schema changes (for ``run_sql``, ``run_code`` or
        ``separate`` operations you add by hand)."""
        return self._make(name, empty, False)

    def _make(self, name: str | None, empty: bool, baseline: bool) -> Migration | None:
        schema = self.schema
        made = schema.make_migration(str(self.directory), name, empty, self.online, baseline, self.name_template)
        if made is None:
            return None
        return Migration(made, self.directory / f"{made}.json", schema)

    def sqlmigrate(self, name: str, *, down: bool = False) -> str:
        """The numbered operations of a migration with their SQL, in one direction."""
        return str(self.schema.sqlmigrate(str(self.directory), name, down))

    def squash(self, *, to: str, name: str | None = None) -> Migration:
        """Fold the migrations 1..``to`` (a number or a name) into one file.

        The first run of schema operations becomes one diff from an empty database. A ``run_sql`` or
        ``run_code`` with ``elidable: true`` is dropped; one without it stops the squash. ``separate``,
        ``commit`` and the operations after them stay as written.
        The new file lists the replaced migrations; a database that applied all of them adopts it with no SQL.
        Keep the old files until every database has passed them. Reads no database."""
        found, path = self.schema.squash_migration(str(self.directory), to, name, self.name_template)
        return Migration(found, Path(path), self.schema)


@dataclass(frozen=True)
class Pulled:
    """A schema file read from a live database (:func:`pull`)."""

    schema: str
    #: What the schema leaves out (rules, views, unsupported types, ...), one line each.
    gaps: list[str]
    #: What a migration from ``schema`` would still change on the database; empty when
    #: the schema reproduces it.
    differences: list[Step]
    #: The SQL body of each view, by its path (``views/<db schema>/<Name>.sql``) next to the schema file.
    views: dict[str, str] = field(default_factory=dict)

    def write(self, path: str | os.PathLike[str]) -> None:
        """Writes the schema file and the view bodies next to it."""
        target = Path(path)
        target.write_text(self.schema)
        for name, body in self.views.items():
            sidecar = target.parent / name
            sidecar.parent.mkdir(parents=True, exist_ok=True)
            sidecar.write_text(body)


async def pull(db: Database) -> Pulled:
    """Reads the database ``db`` connects to (Postgres: its current schema) as a schema
    file, and checks the result by creating it again in a shadow schema that is rolled
    back (SQLite: an in-memory database)."""
    schema, gaps, steps, views = await db._engine.pull_schema(resolve_extensions())
    return Pulled(schema, gaps, [Step(*step) for step in steps], dict(views))


@dataclass(frozen=True)
class Drift:
    """The live database against the state that the migrations give (:meth:`Migrator.drift`)."""

    #: The migration compared with, or None for an empty directory.
    migration: str | None
    #: Steps that bring the database to the snapshot; empty when they match.
    steps: list[Step]
    #: Live objects drift does not compare (rules, views, ...).
    gaps: list[str]

    def __bool__(self) -> bool:
        return bool(self.steps)


_Code = Callable[[Database], Awaitable[Any]]


def _load_code(directory: Path, entry: str) -> _Code:
    """The function of a ``file:function`` entry (the file is relative to ``directory``)."""
    file_name, _, func = entry.rpartition(":")
    file = directory / file_name
    if not file_name or not file.is_file():
        raise MigrationError(f"{entry}: the script {file} does not exist")
    # unique per file, so two directories with the same file names do not share it
    name = f"monorm_migration_{hashlib.sha256(os.fsencode(file.resolve())).hexdigest()[:12]}_{file.stem}"
    spec = importlib.util.spec_from_file_location(name, file)
    if spec is None or spec.loader is None:
        raise MigrationError(f"{file}: can't be loaded")
    module = importlib.util.module_from_spec(spec)
    # dataclasses and pickle look the module up in sys.modules while it runs
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    finally:
        del sys.modules[name]
    run = getattr(module, func, None)
    if not inspect.iscoroutinefunction(run):
        raise MigrationError(f"{entry}: needs `async def {func}(db)`")
    return run


def has_code(directory: str | os.PathLike[str]) -> bool:
    """Whether a migration of ``directory`` has a ``run_code`` operation."""
    return any('"run_code"' in Path(path).read_text() for _, path in _native.list_migrations(os.fspath(directory)))


@dataclass
class _Loaded:
    """A migration plan with the code of its ``run_code`` operations, loaded before anything runs."""

    plan: _native.MigrationPlan
    code: dict[tuple[int, int], _Code]  # (group, item) -> function


def _load_plans(directory: Path, plans: list[_native.MigrationPlan]) -> list[_Loaded]:
    loaded = []
    for plan in plans:
        code: dict[tuple[int, int], _Code] = {}
        for k, (_, items) in enumerate(plan.groups):
            for i, (_, _, entry) in enumerate(items):
                if entry is not None:
                    code[(k, i)] = _load_code(directory, entry)
        loaded.append(_Loaded(plan, code))
    return loaded


async def migrate_command(
    schema: str | os.PathLike[str],
    directory: str | os.PathLike[str],
    url: str,
    target: str | None,
    allow_destructive: bool = False,
    ops: str | None = None,
) -> int:
    """``python -m monorm migrate`` for a directory with ``run_code`` operations: applies with
    :meth:`Migrator.upgrade`. The scripts of the pending migrations are imported
    first, so the models they import are in the default registry when the database
    connects."""
    from .model import registry, load

    dialect = "sqlite" if url.startswith("sqlite:") else None
    probe = await monorm_connect(url, max_connections=1, default=False, registry=Registry(dialect=dialect))
    try:
        loaded = await Migrator(probe, Migrations(directory, schema))._load_up(target, allow_destructive, ops)
    finally:
        await probe.close()
    if not any(registry) and Path(schema).is_file():
        load(schema)
    if any(registry):
        db = await monorm_connect(url, max_connections=2)
    else:
        # scripts that use db.execute only, and no schema file (e.g. a deploy image)
        db = await monorm_connect(url, max_connections=2, registry=Registry(dialect=dialect))
    try:
        done = await Migrator(db, Migrations(directory, schema))._run(loaded, allow_destructive)
    finally:
        await db.close()
    for m in done:
        print(f"Applied {m.name}")
    if not done:
        print("Nothing to apply.")
    return 0


@dataclass(frozen=True)
class Status:
    migration: Migration
    applied: bool
    applied_at: str | None
    #: ``(done, total)`` operations of a migration that stopped part way, else None.
    partial: tuple[int, int] | None = None


class Migrator:
    """Applies and reverts the migrations of a directory on a database.

    A migration runs in one transaction by default (on a connection of the pool, not in the
    caller's transaction) together with its ``monorm_migrations`` row, under an advisory lock: a
    failing migration leaves the database at the previous one, and concurrent migrators apply each
    migration once. A ``commit`` operation ends a transaction early. An ``"atomic": false`` operation
    runs alone, outside a transaction; a failure there leaves the migration partial (see ``ops``).
    """

    def __init__(self, db: Database, migrations: Migrations) -> None:
        self.db = db
        self.migrations = migrations

    @property
    def _dir(self) -> str:
        return str(self.migrations.directory)

    def _migration(self, name: str) -> Migration:
        return Migration(name, self.migrations.directory / f"{name}.json")

    async def status(self) -> list[Status]:
        rows = await self.db._engine.migration_status(self._dir)
        return [Status(self._migration(name), applied, at, partial) for name, applied, at, partial in rows]

    async def upgrade(
        self, target: str | None = None, *, allow_destructive: bool = False, ops: str | None = None
    ) -> list[Migration]:
        """Apply pending migrations up to and including ``target`` (default: all).

        An operation that drops a table or a column, or changes a column type so that data
        can be lost, raises :class:`MigrationError` when the table has rows, unless
        ``allow_destructive=True``. A missing or empty table passes.

        A ``run_code`` operation names ``file:function`` (``async def function(db)``). It runs between
        the schema operations in the same transaction and under the same lock; queries on ``db`` inside
        it go into that transaction. An error rolls back its group and the record.

        ``ops="2-4"`` (or ``"3-"``) runs only those operations (numbered from 1) of the one migration
        ``target``; it continues a partial migration."""
        return await self._run(await self._load_up(target, allow_destructive, ops), allow_destructive)

    async def _load_up(self, target: str | None, allow_destructive: bool, ops: str | None) -> list[_Loaded]:
        plans = await self.db._engine.migration_plan_up(self._dir, target, allow_destructive, ops)
        return _load_plans(self.migrations.directory, plans)

    async def _run(self, loaded: list[_Loaded], allow_destructive: bool = False) -> list[Migration]:
        done = []
        for item in loaded:
            if await self._run_plan(item, allow_destructive):
                done.append(self._migration(item.plan.name))
        return done

    async def _run_plan(self, item: _Loaded, allow_destructive: bool) -> bool:
        """Runs the groups of one plan; False when a concurrent migrator did the work."""
        engine = self.db._engine
        plan = item.plan
        groups = plan.groups
        if not groups:
            await engine.migration_record_empty(plan)
            return True
        for k, (atomic, items) in enumerate(groups):
            if not atomic:
                if not await engine.migration_run_outside(plan, k, allow_destructive):
                    return False
                continue
            tx = await engine.migration_begin_group(plan, k, allow_destructive)
            if tx is None:
                return False
            root = self.db._root
            token = _current_tx.set({**_current_tx.get(), root: tx})
            callbacks = _Callbacks()
            cb_token = _callbacks.set({**_callbacks.get(), root: callbacks})
            try:
                for i in range(len(items)):
                    run = item.code.get((k, i))
                    if run is None:
                        await engine.migration_run_item(tx, plan, k, i)
                    else:
                        await run(self.db)
            except BaseException:
                with contextlib.suppress(Exception):
                    await tx.rollback()
                raise
            finally:
                callbacks.ended = True
                _callbacks.reset(cb_token)
                _current_tx.reset(token)
            await self.db._restore_cause(engine.migration_finish_group(tx, plan, k), tx)
            await self.db._after_commit(callbacks, True)
        return True

    async def drift(self) -> Drift:
        """Compares the database with the schema the migrations reach. That schema is
        created in a shadow (a Postgres schema in a transaction that is rolled back, or an
        in-memory SQLite database) and read back, so both sides use the database's text."""
        migration, steps, gaps = await self.db._engine.migration_drift(self._dir)
        return Drift(migration, [Step(*step) for step in steps], gaps)

    async def baseline(self) -> Migration:
        """Marks the first migration as applied and does not run it, for a database that
        already has the schema (after :func:`pull`). Writes the first migration from the
        schema when the directory has none. Fails once any migration is applied."""
        if not self.migrations.all():
            await self.status()  # fails first if the database has applied migrations
            self.migrations._make(None, False, True)
        name = await self.db._engine.migrate_baseline(self._dir)
        return self._migration(name)

    async def downgrade(
        self,
        steps: int = 1,
        *,
        target: str | None = None,
        force: bool = False,
        allow_destructive: bool = False,
        migration: str | None = None,
        ops: str | None = None,
    ) -> list[Migration]:
        """Revert the last ``steps`` applied migrations, or every one after ``target``
        (``target="zero"`` reverts all). Operations revert from the last to the first. A baselined
        migration (:meth:`baseline`) is refused unless ``force``: reverting it would drop the live
        tables. Every reverse is resolved before the first change: a ``run_sql`` without
        ``reverse_sql`` or a ``run_code`` without ``reverse`` is refused with its operation number.

        ``migration`` with ``ops="2-4"`` reverts only those operations of that one migration."""
        slice_ = None if migration is None and ops is None else (migration or "", ops or "")
        plans = await self.db._engine.migration_plan_down(
            self._dir, max(steps, 0), target, force, allow_destructive, slice_
        )
        loaded = _load_plans(self.migrations.directory, plans)
        return await self._run(loaded, allow_destructive)
