"""Explicit to-one loads, sharing the owner's existing relation cache."""

from __future__ import annotations

import asyncio
from typing import Any

from . import debug
from .errors import IntegrityError, NotLoaded
from .fields import BelongsTo, HasOne


_MISSING = object()


def peek_key(owner: Any, names: tuple[str, ...]) -> Any:
    """A key value (a tuple for several fields), or ``_MISSING`` when a part is not loaded."""
    data = owner.__dict__
    internal = data.get("_orm_internal", {})
    values = tuple(data[n] if n in data else internal.get(n, _MISSING) for n in names)
    if any(v is _MISSING for v in values):
        return _MISSING
    if len(values) == 1:
        return values[0]
    return None if any(v is None for v in values) else values


def key_value(owner: Any, names: tuple[str, ...]) -> Any:
    value = peek_key(owner, names)
    if value is _MISSING:
        raise NotLoaded(f"{owner._meta.name}.{', '.join(names)} is not loaded")
    return value


def _sources(relation: Any) -> tuple[str, ...]:
    return relation.via_fields if isinstance(relation, BelongsTo) else relation.from_fields


class ReferenceBelongsTo(BelongsTo[Any, Any]):
    optional: bool = False

    def __get__(self, obj: Any, owner: Any) -> Any:
        if obj is None:
            return super().__get__(obj, owner)
        data = obj.__dict__
        key = key_value(obj, self.via_fields)
        if self.name in data:
            value = data[self.name]
            keys = data.get("_reference_keys", {})
            absent_is_valid = (
                key is None
                or (self.optional and self.name not in keys)
                or keys.get(self.name, _MISSING) == key
                or owner._meta.registry.get(self.target_name)._meta.filtered
            )
            if value is None and absent_is_valid:
                return None
            if value is not None:
                target_key = peek_key(value, self.to_fields)
                if target_key == key or (target_key is _MISSING and keys.get(self.name, _MISSING) == key):
                    return value
        elif key is None:
            return None
        hint = f"use await obj.load({owner.__name__}.{self.name})"
        if key is not None and (
            (self.name in data and data[self.name] is None)
            or data.get("_reference_absent", {}).get(self.name, _MISSING) == key
        ):
            raise NotLoaded(
                f"{owner.__name__}.{self.name} is not loaded: the key is set and no row of the target has it; {hint}"
            )
        raise NotLoaded(f"{owner.__name__}.{self.name} is not loaded; {hint}")


def install(model: Any) -> None:
    for name, relation in model._meta.relations.items():
        if not isinstance(relation, (BelongsTo, HasOne)):
            continue
        if isinstance(relation, BelongsTo) and not isinstance(relation, ReferenceBelongsTo):
            specialized = ReferenceBelongsTo.__new__(ReferenceBelongsTo)
            specialized.__dict__.update(relation.__dict__)
            relation = specialized
            model._meta.relations[name] = relation
            setattr(model, name, relation)
        if isinstance(relation, ReferenceBelongsTo):
            relation.optional = any(model._meta.fields[n].nullable for n in relation.via_fields)


async def load_reference(owner: Any, name: str, *, reload: bool = False) -> Any:
    from .db import resolve

    from .model import key_match

    relation = owner._meta.relations[name]
    source = _sources(relation)
    key = key_value(owner, source)
    data = owner.__dict__
    if not reload:
        try:
            return getattr(owner, name)
        except NotLoaded:
            pass
    db = resolve(data.get("_db"))
    if reload:
        db = db.primary
    context = (key, db, db._tx())

    async def fetch() -> Any:
        target = relation.target
        to = relation.to_fields if isinstance(relation, BelongsTo) else relation.via_fields
        match = key_match(target, (), [target._meta.fields[n] for n in to], key)
        query = target.objects.using(db).filter(match)
        filtered = query._read_defaults()["filtered"]
        value = None if key is None else await query.first()
        nullable = any(owner._meta.fields[n].nullable for n in source)
        if value is None and isinstance(relation, BelongsTo) and not nullable and not filtered:
            raise IntegrityError(f"{owner._meta.name}.{name}: required target {relation.target_name} is missing")
        return value

    def publish(value: Any) -> None:
        if key_value(owner, source) == key:
            data.get("_reference_absent", {}).pop(name, None)
            data[name] = value
            data.setdefault("_reference_keys", {})[name] = key

    if debug._scope.get() is not None:
        # The loader runs `fetch` in its own task, so the call site is captured here.
        with debug.relation_load(owner._meta.name, name, "load"):
            return await coalesced_load(owner, name, context, fetch, publish)
    return await coalesced_load(owner, name, context, fetch, publish)


async def coalesced_load(owner: Any, name: str, context: tuple[Any, ...], fetch: Any, publish: Any) -> Any:
    """Internal seam for ordinary/generic references: context includes keys, DB and TX.

    The caller validates dependencies and owns its cache policy. Invalidation removes
    the name's token/pending entry. A cancelled waiter leaves shared work running.
    """
    data = owner.__dict__
    pending = data.setdefault("_reference_pending", {})
    entry = pending.get(name)
    if entry is not None and entry[0] == context and not entry[1].done():
        return await asyncio.shield(entry[1])
    tokens = data.setdefault("_reference_tokens", {})
    token = object()
    tokens[name] = token

    async def run() -> Any:
        try:
            value = await fetch()
            if tokens.get(name) is token:
                publish(value)
            return value
        finally:
            if pending.get(name, (None, None))[1] is asyncio.current_task():
                pending.pop(name, None)

    task = asyncio.create_task(run())
    task.add_done_callback(lambda done: None if done.cancelled() else done.exception())
    pending[name] = (context, task)
    return await asyncio.shield(task)


def invalidate_reference(owner: Any, name: str) -> None:
    """Invalidate a selected ordinary/generic reference without cancelling its waiters."""
    data = owner.__dict__
    data.pop(name, None)
    for storage in ("_reference_keys", "_reference_tokens", "_reference_pending", "_reference_absent"):
        data.get(storage, {}).pop(name, None)
