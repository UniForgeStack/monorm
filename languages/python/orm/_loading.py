"""Additive instance loading using the queryset's load vocabulary."""

from __future__ import annotations

from typing import Any

from .errors import NotLoaded
from .expr import ColumnRef, Labeled, RelationPath
from .fields import BelongsTo, HasOne


def loaded(
    owner: Any, path: tuple[str, ...], field: str | None = None, *, label: str | None = None, query: Any = None
) -> bool:
    if not path and query is not None:
        names = query._model_fields if query._model_fields is not None else owner._meta.field_names
        return all(n in owner.__dict__ for n in names) and all(item_loaded(owner, i) for i in query._loads)
    if not path:
        return (
            field in owner.__dict__ if field is not None else all(n in owner.__dict__ for n in owner._meta.field_names)
        )
    name, *rest = path
    relation = owner._meta.relations[name]
    attr = label if not rest and label is not None else name
    try:
        value = owner.__dict__[attr] if label is not None and not rest else getattr(owner, name)
        if not isinstance(relation, (BelongsTo, HasOne)) and attr == name:
            value = value.cached
    except (NotLoaded, KeyError):
        return False
    values = value if isinstance(value, list) else [value]
    return all(v is None or loaded(v, tuple(rest), field, label=label, query=query) for v in values)


def item_loaded(owner: Any, item: Any) -> bool:
    from .query import QuerySet

    if isinstance(item, Labeled):
        return item._name in owner.__dict__

    path = item._via if isinstance(item, QuerySet) else item
    assert path is not None
    return loaded(
        owner,
        path._path,
        path._field.name if isinstance(path, ColumnRef) else None,
        label=item._label if isinstance(item, QuerySet) else None,
        query=item if isinstance(item, QuerySet) else None,
    )


def merge(owner: Any, fresh: Any, reload: bool, seen: set[tuple[int, int]] | None = None) -> None:
    """Preserve existing objects and columns while filling a fetched shape."""
    from .model import Model, _reference_adapter

    if seen is None:
        seen = set()
    pair = (id(owner), id(fresh))
    if pair in seen or owner is fresh:
        return
    seen.add(pair)
    data = owner.__dict__
    for name, value in fresh.__dict__.items():
        if name in (
            "_db",
            "_orm_internal",
            "_reference_keys",
            "_reference_pending",
            "_reference_tokens",
            "_reference_absent",
        ):
            continue
        old = data.get(name)
        if name in owner._meta.fields and not reload and name in data:
            continue
        if isinstance(old, Model) and isinstance(value, Model) and type(old) is type(value) and old.pk == value.pk:
            merge(old, value, reload, seen)
        elif isinstance(old, list) and isinstance(value, list):
            cached_rows, targets = (old, value) if reload else (value, old)
            by_key = {(type(v), v.pk): v for v in cached_rows if isinstance(v, Model)}
            rows = []
            for v in targets:
                prior = by_key.get((type(v), v.pk)) if isinstance(v, Model) else None
                if prior is not None:
                    merge(prior if reload else v, v if reload else prior, reload, seen)
                rows.append(prior if reload and prior is not None else v)
            data[name] = rows
        elif reload or name not in data:
            if name in owner._meta.fields and data.get(name, data.get("_orm_internal", {}).get(name)) != value:
                for rel_name, rel in owner._meta.relations.items():
                    sources = rel.via_fields if isinstance(rel, BelongsTo) else getattr(rel, "from_fields", ())
                    if name in sources:
                        if _reference_adapter is not None:
                            _reference_adapter.invalidate_reference(owner, rel_name)
                        else:
                            data.pop(rel_name, None)
            data[name] = value
    for name in owner._meta.relations:
        if name in fresh.__dict__:
            data.get("_reference_absent", {}).pop(name, None)
    if "_reference_absent" in fresh.__dict__:
        data.setdefault("_reference_absent", {}).update(fresh.__dict__["_reference_absent"])
    if "_orm_internal" in fresh.__dict__:
        internal = data.setdefault("_orm_internal", {})
        for name, value in fresh.__dict__["_orm_internal"].items():
            internal.setdefault(name, value)
    if all(n in data for n in owner._meta.field_names):
        data.pop("_orm_internal", None)


def _primary(owner: Any) -> Any:
    from .db import resolve

    return resolve(owner.__dict__.get("_db")).primary


async def load_instance(owner: Any, items: tuple[Any, ...], reload: bool) -> None:
    from .model import _peek, _reference_adapter
    from .query import QuerySet, prefetch

    model = type(owner)
    # Validate the whole request before doing I/O, including relation query options.
    # Bare bypass on purpose: this only validates the request and runs no query.
    model.objects.without_defaults().load(*items)._select_ir("select", [])
    requested = items or tuple(getattr(model, name) for name in owner._meta.field_names)
    own = [
        item
        for item in requested
        if (isinstance(item, ColumnRef) and not item._path and (reload or item._field.name not in owner.__dict__))
        or (isinstance(item, Labeled) and (reload or item._name not in owner.__dict__))
    ]
    if own and not any(isinstance(i, ColumnRef) for i in own):
        # a labeled aggregate alone must not read every column
        own = [*(getattr(model, f.name) for f in owner._meta.key), *own]
    if own:
        # Bare bypass on purpose: the instance already holds this row, so a default must not hide it from its own load.
        bound = owner.qs().without_defaults().load(*own)
        if reload:
            bound = bound.using("primary")
        if not await bound:
            raise owner.DoesNotExist(f"{model.__name__} {owner.pk!r} no longer exists")
        if reload and _reference_adapter is not None:
            for name, rel in model._meta.relations.items():
                if isinstance(rel, HasOne):
                    _reference_adapter.invalidate_reference(owner, name)
    relations = []
    for item in requested:
        if (isinstance(item, ColumnRef) and not item._path) or isinstance(item, Labeled):
            continue
        path = item._via if isinstance(item, QuerySet) else item
        assert isinstance(path, (ColumnRef, RelationPath))
        if not reload and item_loaded(owner, item):
            continue
        if _reference_adapter is not None and not isinstance(item, (ColumnRef, QuerySet)) and len(path._path) == 1:
            name = path._path[0]
            rel = owner._meta.relations[name]
            if isinstance(rel, (BelongsTo, HasOne)) and (reload or not loaded(owner, path._path)):
                try:
                    cached = getattr(owner, name)
                except NotLoaded:
                    cached = None
                # A partial cached target needs to be filled rather than treated as complete.
                if reload or cached is None:
                    await _reference_adapter.load_reference(owner, name, reload=reload)
                    continue
        relations.append(item)
    if relations:
        staged = object.__new__(model)
        staged.__dict__.update(owner.__dict__)
        keys = {}
        for item in relations:
            path = item._via if isinstance(item, QuerySet) else item
            assert path is not None
            rel = owner._meta.relations[path._path[0]]
            for source in rel.via_fields if isinstance(rel, BelongsTo) else rel.from_fields:
                keys[source] = _peek(owner, source)
        names = {}
        for item in relations:
            path = item._via if isinstance(item, QuerySet) else item
            assert path is not None
            names[id(item)] = (
                item._label if isinstance(item, QuerySet) and item._label and len(path._path) == 1 else path._path[0]
            )
        # A load that leaves a name unset must not keep the old cached value.
        for name in names.values():
            staged.__dict__.pop(name, None)
        await prefetch([staged], *relations, using=_primary(owner) if reload else None)
        # Publish only the fetched roots/labels; staging protects the existing cache on failure.
        fresh_relations = object.__new__(model)
        for item in relations:
            path = item._via if isinstance(item, QuerySet) else item
            assert path is not None
            rel = owner._meta.relations[path._path[0]]
            sources = rel.via_fields if isinstance(rel, BelongsTo) else rel.from_fields
            if any(_peek(owner, source) != keys[source] for source in sources):
                continue
            name = names[id(item)]
            if name in staged.__dict__:
                fresh_relations.__dict__[name] = staged.__dict__[name]
            else:
                owner.__dict__.pop(name, None)
            markers = owner.__dict__.setdefault("_reference_absent", {})
            markers.pop(name, None)
            if name in staged.__dict__.get("_reference_absent", {}):
                markers[name] = staged.__dict__["_reference_absent"][name]
        merge(owner, fresh_relations, reload)
