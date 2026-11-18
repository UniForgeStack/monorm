"""History language members for models with ``@@history.track`` or ``@@history.versions``: ``Model.history``."""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass
import decimal
import inspect
import uuid
import weakref
from datetime import date, datetime, time, timezone
from typing import Any

from monorm import QuerySet, allow_writes
from monorm.db import Database, resolve
from monorm.ext import ModelT, behavior_records, canonical_hash, database_of, json_diff, key_condition

__all__ = ["History", "HistoryEntry", "HistoryInstance", "HistoryQuerySet"]

_SOURCE = "monorm.history"
RESTORE = "restore"
# A version that restores or deletes is always written: a restore must leave its trace, and a delete is an event of its own.
_ALWAYS_WRITTEN = ("delete", RESTORE)
_RECORD_TYPES = ("create", "update", "delete")

# The functions of `register`, by version model class.
_REGISTERED: weakref.WeakKeyDictionary[type, tuple[Callable[..., Any], Callable[..., Any]]] = weakref.WeakKeyDictionary()


async def _call(fn: Callable[..., Any], *args: Any) -> Any:
    """The result of ``fn(*args)``; a function may be sync or async."""
    result = fn(*args)
    return await result if inspect.isawaitable(result) else result


@dataclass(frozen=True)
class HistoryEntry:
    """One event of a timeline. ``diff`` is ``{field: {"old": .., "new": ..}}`` against the previous
    event of the same row; it is ``None`` for the first event and when ``diff=True`` was not asked."""

    event: Any
    diff: dict[str, dict[str, Any]] | None = None


def _decode(type_name: str, value: Any) -> Any:
    """The Python value of a JSON value that an event holds: dates and numbers are text or numbers in JSON."""
    if value is None or type_name == "json":
        return value
    if isinstance(value, list):
        return [_decode(type_name, v) for v in value]
    if type_name == "date_time" and isinstance(value, str):
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed if parsed.tzinfo else parsed.replace(tzinfo=timezone.utc)
    if type_name == "date" and isinstance(value, str):
        return date.fromisoformat(value)
    if type_name == "time" and isinstance(value, str):
        return time.fromisoformat(value)
    if type_name == "uuid" and isinstance(value, str):
        return uuid.UUID(value)
    if type_name == "decimal" and not isinstance(value, decimal.Decimal):
        return decimal.Decimal(str(value))
    return value


class History:
    """``Model.history``: the events of the trackers of one model."""

    def __init__(self, model: type, db: Database | None = None) -> None:
        self._model = model
        self._db = db

    def using(self, db: Database | None) -> History:
        """The same API on the database ``db``."""
        return History(self._model, db)

    # ---- lookups ----

    def _trackers(self) -> list[dict[str, Any]]:
        records = behavior_records(self._model, "extension_records")
        return [r["data"] for r in records if r["source"] == _SOURCE and r["kind"] == "tracker"]

    def _tracker(self, name: str | None) -> dict[str, Any]:
        trackers = self._trackers()
        model = self._model.__name__
        if not trackers:
            raise TypeError(f"{model} has no @@history.track attribute")
        if name is None:
            if len(trackers) > 1:
                names = ", ".join(t["event_model"] for t in trackers)
                raise ValueError(f"{model} has several trackers ({names}); pass tracker=<event model name>")
            return trackers[0]
        for t in trackers:
            if t["event_model"] == name:
                return t
        raise ValueError(f"{model} has no tracker with the event model {name}")

    def _event_class(self, tracker: dict[str, Any]) -> Any:
        return self._model._meta.registry.get(tracker["event_model"])

    def _key(self, row: Any) -> list[Any]:
        """The key values of ``row``: a model object, a dict by field name, a tuple or one value."""
        meta = self._model._meta
        names = [f.name for f in meta.key]
        if isinstance(row, self._model):
            values = [getattr(row, n) for n in names]
        elif isinstance(row, dict):
            values = [row[n] for n in names]
        elif isinstance(row, (tuple, list)):
            values = list(row)
        else:
            values = [row]
        if len(values) != len(names):
            raise ValueError(f"{self._model.__name__} has the key ({', '.join(names)}): got {len(values)} value(s)")
        if any(v is None for v in values):
            raise ValueError(f"{self._model.__name__}: the key of the row has a None value; save the row first")
        return values

    def _database(self, row: Any = None) -> Database:
        # A model object or an event knows the database that loaded it; a key value does not.
        return resolve(self._db or (database_of(row) if hasattr(row, "__dict__") else None))

    def _row_conditions(self, tracker: dict[str, Any], event_class: Any, key: Sequence[Any]) -> list[Any]:
        return [getattr(event_class, k["event_field"]) == v for k, v in zip(tracker["key"], key, strict=True)]

    # ---- writes ----

    async def event(self, row: Any, label: str, *, tracker: str | None = None) -> Any:
        """Write one event of ``row`` now, with the label ``label``. Gives the event."""
        t = self._tracker(tracker)
        db = self._database(row)
        key = self._key(row)
        rows = await db.fetch(t["event_sql"], label, *key)
        if not rows:
            raise LookupError(f"{self._model.__name__} {key[0] if len(key) == 1 else tuple(key)} does not exist")
        event_class = self._event_class(t)
        return await event_class.objects.using(db).get(event_class.id == rows[0]["id"])

    async def backfill(self, label: str = "snapshot", *, tracker: str | None = None) -> int:
        """Write one event per row of the tracked table, in one statement. Gives the number of events."""
        t = self._tracker(tracker)
        rows = await self._database().fetch(t["backfill_sql"], label)
        # PostgreSQL gives one row with the count; SQLite gives one row per event.
        return int(rows[0]["n"]) if rows and "n" in rows[0] else len(rows)

    async def purge(self, *, before: datetime, tracker: str | None = None) -> int:
        """Delete the events older than ``before``: the one delete path of an event model.
        Without ``tracker``, it purges every tracker of the model. Gives the number of events deleted."""
        db = self._database()
        total = 0
        for t in [self._tracker(tracker)] if tracker is not None else self._trackers():
            event_class = self._event_class(t)
            with allow_writes(event_class):
                total += int(await event_class.objects.using(db).filter(event_class.created_at < before).delete())
        return total

    # ---- reads ----

    async def timeline(self, row: Any, *, diff: bool = False, tracker: str | None = None) -> list[HistoryEntry]:
        """The events of ``row`` in order. With ``diff=True``, each entry has the changed fields
        against the previous event of the row. On a version model it gives the versions of the entity by number."""
        if self._version_model() is not None:
            return await self._version_timeline(row, diff)
        t = self._tracker(tracker)
        event_class = self._event_class(t)
        conditions = self._row_conditions(t, event_class, self._key(row))
        events = await event_class.objects.using(self._database(row)).filter(*conditions).order_by(event_class.id)
        entries: list[HistoryEntry] = []
        previous: Any = None
        for event in events:
            changes = json_diff(previous.data, event.data) if diff and previous is not None else None
            entries.append(HistoryEntry(event, changes))
            previous = event
        return entries

    async def at(self, row: Any, when: datetime, *, tracker: str | None = None) -> Any:
        """The last event of ``row`` at or before ``when``, or ``None``. Its ``data`` is the row state then.
        On a version model it is ``version_at``."""
        if self._version_model() is not None:
            return await self.version_at(row, when)
        t = self._tracker(tracker)
        event_class = self._event_class(t)
        conditions = self._row_conditions(t, event_class, self._key(row))
        query = (
            event_class.objects.using(self._database(row))
            .filter(*conditions, event_class.created_at <= when)
            .order_by(event_class.id.desc())
            .first()
        )
        return await query

    async def restore(self, event: Any) -> Any:
        """Write the row back from ``event``, in one transaction, and leave one event with the label
        ``restore``. A row that exists is updated with the fields of the event. A deleted row is inserted
        again, only when the tracker covers every field. Gives the row.
        On a version model it calls the registered ``restore`` function with the snapshot and gives the
        new ``restore`` version."""
        if self._version_model() is not None:
            return await self._restore_version(event)
        event_class = type(event)
        t = next((t for t in self._trackers() if t["event_model"] == event_class.__name__), None)
        if t is None:
            raise TypeError(f"{event_class.__name__} is not the event model of a tracker of {self._model.__name__}")
        model = self._model
        meta = model._meta
        db = self._database(event)
        key = [getattr(event, k["event_field"]) for k in t["key"]]
        match = key_condition(model, key[0] if len(key) == 1 else tuple(key))
        data: dict[str, Any] = dict(event.data)
        known = meta.input_fields
        values = {name: _decode(known[name].type_name, value) for name, value in data.items() if name in known}
        for k, v in zip(t["key"], key, strict=True):
            values.setdefault(k["field"], v)
        async with db.transaction():
            # Bypass read defaults (soft delete, tenant) so a hidden row is not inserted twice.
            rows = model.objects.using(db).without_defaults()
            found = await rows.filter(match).first()
            after = await self._last_id(db, event_class, t, key)
            await self._mark_restore(db, t, t["event_table"])
            if found is not None:
                changes = {n: v for n, v in values.items() if n not in {k["field"] for k in t["key"]}}
                if t["full"]:
                    self._check_required(values, known, event)
                if changes:
                    await rows.filter(match).update(**changes)
                row = await rows.filter(match).first()
            else:
                if not t["full"]:
                    raise ValueError(
                        f"cannot restore {model.__name__} from {event_class.__name__} {event.id}: the row is gone "
                        "and the tracker covers only some fields, so an insert would write a half row"
                    )
                self._check_required(values, known, event)
                row = await rows.insert(**values)
            await self._mark_restore(db, t, "")
            await self._label_restore(db, event_class, t, key, after, row)
        return row

    # ---- versions mode ----

    def _version_model(self) -> dict[str, Any] | None:
        records = behavior_records(self._model, "extension_records")
        return next((r["data"] for r in records if r["source"] == _SOURCE and r["kind"] == "versions"), None)

    def _versions(self) -> dict[str, Any]:
        v = self._version_model()
        if v is None:
            raise TypeError(f"{self._model.__name__} has no @@history.versions attribute")
        return v

    def _functions(self) -> tuple[Callable[..., Any], Callable[..., Any]]:
        found = _REGISTERED.get(self._model)
        if found is None:
            name = self._model.__name__
            raise RuntimeError(f"{name}.history is not registered; call {name}.history.register(generate=..., restore=...) first")
        return found

    def register(self, *, generate: Callable[..., Any], restore: Callable[..., Any]) -> None:
        """Set the two functions of this version model. ``generate(entity)`` gives the snapshot, a dict of
        JSON values; ``restore(snapshot)`` writes it back. Each may be sync or async."""
        self._versions()
        if not callable(generate) or not callable(restore):
            raise TypeError("register needs generate and restore functions")
        _REGISTERED[self._model] = (generate, restore)

    @staticmethod
    def _entity_id(entity: Any) -> Any:
        """The id of the entity: a plain value is the id; an object or a dict gives its ``id``."""
        if isinstance(entity, (int, str, uuid.UUID)):
            return entity
        value = entity.get("id") if isinstance(entity, dict) else getattr(entity, "id", None)
        if value is None:
            raise ValueError("the entity has no id: pass an id value, or an object with an id")
        return value

    def _id_column(self, v: dict[str, Any]) -> Any:
        return getattr(self._model, v["pk_field"])

    async def _write(
        self, db: Database, v: dict[str, Any], entity_id: Any, make_snapshot: Callable[[], Any], change_type: str, restored_from: Any
    ) -> Any:
        model = self._model
        async with db.transaction():
            # The lock comes before the snapshot and the read of the latest version, so a second writer waits for
            # the first one and never writes an older snapshot after a newer one.
            if v["lock_sql"] is not None:
                await db.fetch(v["lock_sql"], entity_id)
            snapshot = await _call(make_snapshot)
            if not isinstance(snapshot, dict):
                raise TypeError(f"generate must give a dict; it gave {type(snapshot).__name__}")
            digest = canonical_hash(snapshot)
            if change_type not in _ALWAYS_WRITTEN:
                latest = await db.fetch(v["latest_sql"], entity_id)
                # A version after a delete is a new life of the entity, so it never matches the delete version.
                if latest and latest[0]["change_type"] != "delete" and latest[0]["content_hash"] == digest:
                    return await model.objects.using(db).get(self._id_column(v) == latest[0]["id"])
            rows = await db.fetch(v["insert_sql"], entity_id, snapshot, digest, change_type, restored_from)
            return await model.objects.using(db).get(self._id_column(v) == rows[0]["id"])

    async def record(self, entity: Any, change_type: str) -> Any:
        """Store the snapshot that ``generate(entity)`` gives as the next version of the entity, with the change
        type ``create``, ``update`` or ``delete``. A snapshot with the hash of the latest version writes nothing
        and gives that version, unless that version is a ``delete``. Inside a transaction, the version rolls back with it. Gives the version."""
        v = self._versions()
        if change_type not in _RECORD_TYPES:
            raise ValueError(f"change_type must be one of {', '.join(_RECORD_TYPES)}; restore() writes the restore version")
        generate, _ = self._functions()
        db = self._database(entity)
        return await self._write(db, v, self._entity_id(entity), lambda: generate(entity), change_type, None)

    def _version_conditions(self, v: dict[str, Any], entity: Any) -> list[Any]:
        return [getattr(self._model, v["of_field"]) == self._entity_id(entity)]

    async def _version_timeline(self, entity: Any, diff: bool) -> list[HistoryEntry]:
        v = self._versions()
        cls = self._model
        versions = await cls.objects.using(self._database(entity)).filter(*self._version_conditions(v, entity)).order_by(cls.version_number)
        entries: list[HistoryEntry] = []
        previous: Any = None
        for version in versions:
            changes = json_diff(previous.snapshot, version.snapshot) if diff and previous is not None else None
            entries.append(HistoryEntry(version, changes))
            previous = version
        return entries

    async def version_at(self, entity: Any, when: datetime) -> Any:
        """The last version of the entity with ``effective_at`` at or before ``when``, or ``None``."""
        v = self._versions()
        cls = self._model
        return await (
            cls.objects.using(self._database(entity))
            .filter(*self._version_conditions(v, entity), cls.effective_at <= when)
            .order_by(cls.version_number.desc())
            .first()
        )

    async def _restore_version(self, version: Any) -> Any:
        v = self._versions()
        if not isinstance(version, self._model):
            raise TypeError(f"{type(version).__name__} is not a version of {self._model.__name__}")
        _, restore = self._functions()
        db = self._database(version)
        entity_id = getattr(version, v["of_field"])
        async with db.transaction():
            await _call(restore, version.snapshot)
            return await self._write(db, v, entity_id, lambda: version.snapshot, RESTORE, getattr(version, v["pk_field"]))

    def _check_required(self, values: dict[str, Any], known: Any, event: Any) -> None:
        missing = [n for n, f in known.items() if n not in values and not f.nullable and not f.has_insert_default]
        if missing:
            raise ValueError(
                f"cannot restore {self._model.__name__} from {type(event).__name__} {event.id}: "
                f"the event has no value for the required field(s) {', '.join(missing)}"
            )

    async def _last_id(self, db: Database, event_class: Any, t: dict[str, Any], key: Sequence[Any]) -> int:
        conditions = self._row_conditions(t, event_class, key)
        last = await event_class.objects.using(db).filter(*conditions).order_by(event_class.id.desc()).first()
        return int(last.id) if last is not None else 0

    async def _mark_restore(self, db: Database, t: dict[str, Any], value: str) -> None:
        """PostgreSQL: set the transaction-local setting that makes the triggers of the tracker write the label
        ``restore``; an empty ``value`` clears it. SQLite triggers cannot read a setting, so they have no such SQL."""
        if t["restore_sql"] is not None:
            await db.fetch(t["restore_sql"], value)

    async def _label_restore(
        self, db: Database, event_class: Any, t: dict[str, Any], key: Sequence[Any], after: int, row: Any
    ) -> None:
        """Leave one ``restore`` event: PostgreSQL triggers wrote it already; on SQLite, the events after ``after``
        get the label here (one transaction writes the row, so they are the restore's). Write one when no trigger did."""
        conditions = self._row_conditions(t, event_class, key)
        events = event_class.objects.using(db)
        if t["restore_sql"] is None:
            with allow_writes(event_class):
                found = await events.filter(*conditions, event_class.id > after).update(label=RESTORE)
        else:
            found = await events.filter(*conditions, event_class.id > after, event_class.label == RESTORE).count()
        if not found:
            await self.using(db).event(row, RESTORE, tracker=t["event_model"])


class HistoryQuerySet(QuerySet[ModelT]):
    """No members: ``Model.history`` is an instance member."""


class HistoryInstance:
    """The instance mixin: ``Model.history`` and ``row.history``."""

    class _Descriptor:
        def __get__(self, obj: Any, owner: type) -> History:
            return History(owner, database_of(obj) if obj is not None else None)

    history = _Descriptor()

