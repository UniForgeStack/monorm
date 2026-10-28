"""Native engine (Rust, PyO3). Internal: use the ``monorm`` package API instead."""

from collections.abc import Awaitable, Sequence
from pathlib import Path
from typing import Any, final

@final
class _Default: ...

DEFAULT: _Default

class DatabaseError(Exception):
    sqlstate: str | None
    constraint: str | None
    detail: str | None

class IntegrityError(DatabaseError): ...
class LockNotAvailable(DatabaseError): ...
class TransactionAborted(DatabaseError): ...
class QueryError(ValueError): ...
class SchemaError(ValueError): ...
class SchemaWarning(UserWarning): ...
class MigrationError(Exception): ...
class WriteProtected(Exception): ...

def prepare_schema(schema_json: str, context_json: str | None = None) -> str: ...
def native_artifact() -> str: ...
def compile_schema(source: str, path: str | None = None, extensions: dict[str, str] | None = None) -> str: ...
def compile_schema_file(path: str, apps: dict[str, str] | None = None, extensions: dict[str, str] | None = None) -> str: ...
def generate_python(path: str, query_sets: dict[str, str] | None = None, model_methods: dict[str, str] | None = None, apps: dict[str, str] | None = None, extensions: dict[str, str] | None = None) -> tuple[str, str]: ...
def cli(argv: list[str]) -> int: ...
def cli_schema(argv: list[str]) -> Path: ...
def cli_migrate_args(argv: list[str]) -> tuple[Path, Path, str | None, str | None] | None: ...
def list_migrations(dir: str) -> list[tuple[str, Path]]: ...
def find_migration(dir: str, name: str) -> tuple[str, Path]: ...

@final
class Schema:
    def inspect(self, op_json: str, params: list[Any]) -> str: ...
    def inspect_insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        options: str,
        params: list[Any],
        batch_size: int | None = None,
        returning: bool = False,
    ) -> str: ...
    def inspect_update_many(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        filters_json: str,
        params: list[Any],
        batch_size: int | None = None,
        returning: bool = False,
        without_defaults: str = "false",
    ) -> str: ...
    def __init__(self, schema_json: str, classes: dict[str, type] | None = None, packages: dict[str, str] | None = None) -> None: ...  # no `packages`: hook bindings stay unresolved
    def validate_insert(self, model: str, fields: list[str], rows: list[list[Any]], allowed: list[str]) -> None: ...
    def unique_row_update(self, op_json: str, params: list[Any], allowed: list[str]) -> bool: ...
    def sql(self, op_json: str, params: list[Any]) -> str: ...
    def read_flags(self) -> str: ...
    def effective_order(self, select_json: str) -> str: ...
    def current_filter(self, select_json: str) -> str: ...
    def statement(self, op_json: str, params: list[Any]) -> str: ...
    def update_many_sql(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        filters_json: str = "[]",
        params: list[Any] = ...,
        batch_size: int | None = None,
        disable: list[str] = ...,
    ) -> list[str]: ...
    def ddl(self) -> list[str]: ...
    def snapshot(self) -> str: ...
    def migration(self, previous: str | None = None) -> str: ...
    def plan_migration(self, dir: str) -> str: ...
    def make_migration(self, dir: str, name: str | None = None, empty: bool = False) -> str | None: ...

@final
class Transaction:
    def commit(self) -> Awaitable[None]: ...
    def rollback(self) -> Awaitable[None]: ...
    def mark_failed(self, message: str, cause: BaseException | None = None) -> None: ...
    def restore_cause(self, error: BaseException) -> None: ...

@final
class Trace:
    def __init__(self) -> None: ...
    def take(self) -> list[tuple[str, float, float, int, str | None]]: ...

@final
class SessionLock:
    def release(self, trace: Trace | None = None) -> Awaitable[None]: ...

@final
class Engine:
    def inspect(self, op_json: str, params: list[Any]) -> str: ...
    def inspect_insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        options: str,
        params: list[Any],
        batch_size: int | None = None,
        returning: bool = False,
    ) -> str: ...
    def inspect_update_many(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        filters_json: str,
        params: list[Any],
        batch_size: int | None = None,
        returning: bool = False,
        without_defaults: str = "false",
    ) -> str: ...
    def prefetch(
        self,
        op_json: str,
        params: list[Any],
        keys: list[str],
        rows: list[list[Any]],
        parents: list[Any],
        tx: Transaction | None = None,
        db: Any = None,
        trace: Trace | None = None,
    ) -> Awaitable[None]: ...
    def run(
        self,
        op_json: str,
        params: list[Any],
        tx: Transaction | None = None,
        row_cls: type | None = None,
        db: Any = None,
        allowed: Sequence[str] = ...,
        trace: Trace | None = None,
    ) -> Awaitable[Any]: ...
    def insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        conflict: list[str] | None = None,
        update: list[str] | None = None,
        set: str | None = None,
        params: list[Any] = ...,
        tx: Transaction | None = None,
        db: Any = None,
        allowed: Sequence[str] = ...,
        batch_size: int | None = None,
        conflict_indexes: Sequence[str] | None = None,
        trace: Trace | None = None,
        returning: bool = True,
        scope: str | None = None,
        without_defaults: str = "false",
    ) -> Awaitable[Any]: ...
    def copy_insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        tx: Transaction | None = None,
        allowed: Sequence[str] = ...,
        trace: Trace | None = None,
    ) -> Awaitable[int]: ...
    def attach(
        self,
        model: str,
        parent_id: Any,
        fields: list[str],
        rows: list[list[Any]],
        tx: Transaction | None = None,
        db: Any = None,
        allowed: Sequence[str] = ...,
        trace: Trace | None = None,
    ) -> Awaitable[list[Any]]: ...
    def update_many(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        filters_json: str,
        params: list[Any],
        returning: bool = False,
        batch_size: int | None = None,
        tx: Transaction | None = None,
        db: Any = None,
        without_defaults: str = "false",
        allowed: Sequence[str] = ...,
        trace: Trace | None = None,
    ) -> Awaitable[Any]: ...
    def begin(self, tx: Transaction | None = None) -> Awaitable[Transaction]: ...
    def advisory_lock(
        self, key: int, name: bytes | None, exclusive: bool, nowait: bool, tx: Transaction, trace: Trace | None = None
    ) -> Awaitable[bool]: ...
    def with_settings(self, names: list[str], values: list[str]) -> Engine: ...
    def session_lock(
        self,
        key: int,
        name: bytes | None,
        exclusive: bool,
        nowait: bool,
        timeout_ms: int | None = None,
        trace: Trace | None = None,
    ) -> Awaitable[SessionLock | None]: ...
    def execute(self, sql: str, tx: Transaction | None = None, trace: Trace | None = None) -> Awaitable[int]: ...
    def execute_script(self, statements: list[str], tx: Transaction | None = None) -> Awaitable[None]: ...
    def explain(
        self,
        op_json: str,
        params: list[Any],
        analyze: bool = False,
        tx: Transaction | None = None,
        trace: Trace | None = None,
    ) -> Awaitable[str]: ...
    def fetch(
        self, sql: str, params: list[Any], tx: Transaction | None = None, trace: Trace | None = None
    ) -> Awaitable[list[dict[str, Any]]]: ...
    def fetch_text(self, sql: str, tx: Transaction | None = None) -> Awaitable[list[tuple[str | None, ...]]]: ...
    def create_tables(self) -> Awaitable[None]: ...
    def drop_tables(self) -> Awaitable[None]: ...
    def migration_status(self, dir: str) -> Awaitable[list[tuple[str, bool, str | None]]]: ...
    def migrate_up(self, dir: str, target: str | None = None, force: bool = False) -> Awaitable[list[str]]: ...
    def migrate_down(
        self, dir: str, steps: int = 1, target: str | None = None, force: bool = False
    ) -> Awaitable[list[str]]: ...
    def pull_schema(self) -> Awaitable[tuple[str, list[str], list[tuple[str, str, str | None]]]]: ...
    def migration_drift(
        self, dir: str
    ) -> Awaitable[tuple[str | None, list[tuple[str, str, str | None]], list[str]]]: ...
    def migrate_baseline(self, dir: str) -> Awaitable[str]: ...
    def migration_pending(self, dir: str, target: str | None = None) -> Awaitable[list[tuple[str, Path]]]: ...
    def migration_begin(self, name: str, path: str) -> Awaitable[Transaction | None]: ...
    def migration_finish(self, tx: Transaction, name: str, path: str) -> Awaitable[None]: ...
    def close(self) -> Awaitable[None]: ...

def connect(url: str, schema: Schema, max_connections: int = 10, disable: list[str] = ...) -> Awaitable[Engine]: ...

# Exact statically compiled packaging metadata, JSON ABI 1.
def profile_metadata() -> str: ...
