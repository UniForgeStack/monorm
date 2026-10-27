"""Model base class, the model registry, and models built from a schema file."""

from __future__ import annotations

import copy
import datetime
import decimal
import enum
import json
import types
import uuid
from os import PathLike
from typing import TYPE_CHECKING, Any, ClassVar, cast

from . import _capabilities, _extensions, _native
from ._apps import resolve_apps
from ._packages import package_dirs, resolve_extensions
from .errors import DoesNotExist, MultipleObjectsReturned, NotLoaded, VersionConflict
from collections.abc import Iterable, Sequence

from .expr import ColumnRef, Condition, RelationPath, and_, differs
from .index import _Indexes
from .fields import BY_TYPE, Array, BelongsTo, Enum, Field, HasMany, HasOne, ManyToMany, Relation, String, key_items
from .nested import NestedGroup, NestedJson, NestedType, _Access, build_types, leaf_names

if TYPE_CHECKING:
    from typing_extensions import Self

    from .query import BoundQuerySet, QuerySet

# The native artifact is fixed for the process; read its capabilities once.
_ARTIFACT: dict[str, Any] = json.loads(_native.native_artifact())
_CAPABILITIES: tuple[str, ...] = tuple(_ARTIFACT.get("capabilities", ()))
_capabilities.check_requires(_ARTIFACT)
# Only a composition artifact (schema transformations) can change an already prepared schema.
_REPREPARE: bool = (
    "schema-transformations" in _CAPABILITIES
    or json.loads(_native.profile_metadata())["capabilities"].get("composition") is True
)

_reference_adapter: types.ModuleType | None = None
if "reference-loading" in _CAPABILITIES:
    from . import _references

    _reference_adapter = _references

__all__ = ["Model", "ModelMeta", "Registry", "registry", "column", "define", "describe", "load", "loads"]


_ABSENT = object()


def _detached(value: Any) -> Any:
    """A deep copy; empty containers and ``None`` skip the generic ``deepcopy`` walk."""
    if value is None:
        return None
    return copy.deepcopy(value) if value else type(value)()


def _peek(row: Model, name: str) -> Any:
    """A key value or ``_ABSENT``; an absent key never equals a loaded one."""
    data = row.__dict__
    return data[name] if name in data else data.get("_orm_internal", {}).get(name, _ABSENT)


def _key_fields(name: str, fields: dict[str, Field[Any]], order: Sequence[str] | None) -> tuple[Field[Any], ...]:
    """The primary key fields in key order (``@@id`` order, else declaration order)."""
    pks = [n for n, f in fields.items() if f.primary_key]
    names = list(order) if order else pks
    if not pks or sorted(names) != sorted(pks):
        raise TypeError(
            f"model {name} must declare exactly one primary key: one field, or the fields of @@id in key order"
        )
    return tuple(fields[n] for n in names)


def key_match(model: type[Model], path: tuple[str, ...], fields: Sequence[Field[Any]], value: Any) -> Condition:
    """``field = value`` for each key field; ``value`` is a tuple for several fields."""
    items = key_items(tuple(f.name for f in fields), value)
    return and_(*(ColumnRef(model, path, f) == items[f.name] for f in fields))


class ModelMeta:
    """Schema information about one model (``User._meta``)."""

    def __init__(self, model: type[Model], table: str, registry: Registry, key: Sequence[str] | None = None) -> None:
        self.model = model
        self.name = model.__name__
        self.table = table
        self.registry = registry
        # The model's schema IR when it was built from a compiled schema: it carries
        # everything (indexes, triggers, extension types) the Python side doesn't use.
        self.schema_ir: dict[str, Any] | None = None
        # Local fields that `attach` accepts; `None` for a model that is not a child of a multi-table model.
        self.attach_fields: dict[str, Field[Any]] | None = None
        self.fields: dict[str, Field[Any]] = {}
        self.relations: dict[str, Relation[Any, Any]] = {}
        # Nested-type fields: a JSON one is also in `fields`, a `columns` one has leaf fields there.
        self.nested: dict[str, _Access] = {}
        for klass in reversed(model.__mro__):
            for attr, value in vars(klass).items():
                if isinstance(value, Field):
                    self.fields[attr] = value
                elif isinstance(value, Relation):
                    self.relations[attr] = value
                if isinstance(value, _Access):
                    self.nested[attr] = value
        self.key: tuple[Field[Any], ...] = _key_fields(self.name, self.fields, key)
        # Row tuples from the engine follow this order.
        self.field_names: tuple[str, ...] = tuple(self.fields)
        self.input_fields: dict[str, Field[Any]] = self.fields
        self.leaf_group: dict[str, str] = {}
        self.public_names: tuple[str, ...] = self.field_names
        self.decoders: tuple[tuple[str, tuple[str, ...], Any], ...] = ()
        self.index_nested()
        # The engine says a default read of this model can hide rows.
        self.filtered: bool = False
        # Fields whose loaded value an instance update or delete must match (`instance_guards`).
        self.guards: tuple[str, ...] = ()
        # Fields that the default `on_conflict(update=True)` list leaves out (`write_assignments`).
        self.upsert_drop: frozenset[str] = frozenset()
        # The unique indexes `Model.indexes` offers (`on_conflict(index=...)`).
        self.index_names: tuple[str, ...] = ()
        # Fields that a proxy declares non-null over a nullable stored column.
        self.narrowed: frozenset[str] = frozenset()
        # The class of `objects` (`use_query_set`): `None` for QuerySet, or a "module:Class"
        # path until first use; and the relation sets mixed with it, by base class.
        self.query_set: type[QuerySet[Any]] | str | None = None
        self.related_sets: dict[type, type] = {}
        # The query-set classes of extensions in MRO order, and the class composed from them
        # with `query_set` (built on first use).
        self.extension_sets: tuple[type[QuerySet[Any]], ...] = ()
        self.composed_set: type[QuerySet[Any]] | None = None
        # The bound-safe query-set classes of extensions (same order) and their composed class.
        self.bound_extension_sets: tuple[type[BoundQuerySet[Any]], ...] = ()
        self.composed_bound: type[BoundQuerySet[Any]] | None = None

    def index_nested(self) -> None:
        """The leaf-to-group map, the instance attribute names and the row decoders; run again when ``fields`` changes."""
        groups = {n: g for n, g in self.nested.items() if not isinstance(g, Field)}
        groups.update({n: f for n, f in self.fields.items() if isinstance(f, _Access)})
        self.nested = groups
        self.leaf_group = {n: g.name for g in groups.values() if g.storage == "columns" for n in g.leaf_names}  # type: ignore[attr-defined]
        names: list[str] = []
        for n in self.field_names:
            g = self.leaf_group.get(n, n)
            if g not in names:
                names.append(g)
        self.public_names = tuple(names)
        self.decoders = tuple(g.decoder() for g in groups.values())  # type: ignore[attr-defined]

    def expand(self, names: Iterable[str]) -> list[str]:
        """Engine field names for instance attribute names: a ``columns`` group gives its leaves."""
        out: list[str] = []
        for n in names:
            g = self.nested.get(n)
            out.extend(g.leaf_names if g is not None and g.storage == "columns" else (n,))  # type: ignore[attr-defined]
        return out

    @property
    def key_names(self) -> tuple[str, ...]:
        return tuple(f.name for f in self.key)

    @property
    def pk(self) -> Field[Any]:
        """The primary key field of a model with a one-field key."""
        if len(self.key) != 1:
            raise TypeError(f"{self.name} has a primary key of several fields; use _meta.key")
        return self.key[0]

    @property
    def key_name(self) -> str:
        name: str = self.pk.name
        return name

    def pk_ref(self) -> ColumnRef[Any]:
        return ColumnRef(self.model, (), self.pk)

    def key_refs(self) -> tuple[ColumnRef[Any], ...]:
        return tuple(ColumnRef(self.model, (), f) for f in self.key)

    def ir(self) -> dict[str, Any]:
        if self.schema_ir is not None:
            return self.schema_ir
        out: dict[str, Any] = {
            "name": self.name,
            "table": self.table,
            "fields": [f.ir() for f in self.fields.values()],
            "relations": [r.ir() for r in self.relations.values()],
        }
        if len(self.key) > 1:
            out["primary_key"] = list(self.key_names)
        return out

    def __repr__(self) -> str:
        return f"<ModelMeta {self.name} table={self.table!r}>"


class Registry:
    """A set of models compiled together into one native schema.

    Models join the default :data:`registry` unless told otherwise (``define(...,
    registry=other)``), which is how one process can hold two versions of a schema,
    e.g. in migration tests.
    """

    def __init__(self, *, dialect: str | None = None) -> None:
        self._models: dict[str, type[Model]] = {}
        # Schema enums: their Python classes and IR, by name.
        self._enums: dict[str, type[enum.Enum]] = {}
        self._enum_ir: dict[str, dict[str, Any]] = {}
        # Schema-level IR (extensions, functions, extension catalog) from define().
        self._dialect: str | None = dialect
        self._schema_extra: dict[str, list[Any]] = {}
        self._behavior: dict[str, Any] = {}
        self._identities: dict[str, Any] | None = None
        self._native: _native.Schema | None = None

    def register(self, model: type[Model]) -> None:
        name = model.__name__
        if name in self._models and self._models[name] is not model:
            raise TypeError(f"a model named {name} is already registered")
        self._models[name] = model
        self._native = None

    def get(self, name: str) -> type[Model]:
        try:
            return self._models[name]
        except KeyError:
            raise LookupError(f"unknown model {name!r}; is its module imported?") from None

    def get_enum(self, name: str) -> type[enum.Enum]:
        try:
            return self._enums[name]
        except KeyError:
            raise LookupError(f"unknown enum {name!r}; is its module imported?") from None

    def register_enum(self, ir: dict[str, Any], cls: type[enum.Enum]) -> None:
        name = ir["name"]
        if name in self._enum_ir and self._enum_ir[name] != ir:
            raise TypeError(f"an enum named {name} is already registered")
        self._enums[name] = cls
        self._enum_ir[name] = ir
        self._native = None

    def __iter__(self) -> Any:
        return iter(self._models.values())

    def ir(self) -> dict[str, Any]:
        out: dict[str, Any] = {"models": [m._meta.ir() for m in self._models.values()]}
        if self._enum_ir:
            out["enums"] = list(self._enum_ir.values())
        if self._dialect is not None:
            out["dialect"] = self._dialect
        out.update(self._schema_extra)
        if self._identities is not None:
            out["identities"] = self._identities
        if self._behavior:
            out["behavior"] = self._behavior
        return copy.deepcopy(out)

    def prepare(self) -> _native.Schema:
        """Prepare class declarations after registering a complete dependency batch."""
        return self._prepare(None)

    def _prepare(self, done: tuple[str, dict[str, Any]] | None) -> _native.Schema:
        """``done`` is the prepared JSON and IR these classes were just built from."""
        if self._native is not None:
            return self._native
        snapshot = self._candidate()
        if done is None:
            prepared_json = _native.prepare_schema(json.dumps(snapshot.ir()))
            prepared = json.loads(prepared_json)
        else:
            prepared_json, prepared = done
        classes: dict[str, type] = {**snapshot._models, **snapshot._enums}
        associations: list[
            tuple[type[Model], dict[str, Any], dict[str, Field[Any]], dict[str, Relation[Any, Any]]]
        ] = []
        multi_table = _multi_table(prepared["models"], prepared.get("behavior", {}))
        for ir in prepared["models"]:
            if ir["name"] not in snapshot._models:
                raise TypeError(
                    "class preparation cannot add or rename models; use define() for transformed model identities"
                )
            model = snapshot._models[ir["name"]]
            if model._meta.registry is not self:
                continue
            fields: dict[str, Field[Any]] = {}
            for f in ir["fields"]:
                name = f["name"]
                if name in _RESERVED:
                    raise TypeError(f"{ir['name']}.{name}: reserved model member")
                existing = model._meta.fields.get(name)
                if existing is None and hasattr(model, name):
                    raise TypeError(f"{ir['name']}.{name}: generated field collides with an existing model member")
                field = existing if existing is not None and existing.ir() == f else _field(f, key_default=_key_default(multi_table, ir, f))
                if field is not existing:
                    original = existing or next(
                        (old for old in model._meta.fields.values() if old.column == f["column"]), None
                    )
                    if original is not None and callable(original.default):
                        field.default = original.default
                    field.__set_name__(model, name)
                fields[name] = field
            for old in model._meta.fields.keys() - fields.keys():
                if old not in vars(model):
                    raise TypeError(
                        "class preparation cannot remove inherited members; use define() for this logical view"
                    )
            relations: dict[str, Relation[Any, Any]] = {}
            for r in ir.get("relations", ()):
                name = r["name"]
                existing_relation = model._meta.relations.get(name)
                if name in _RESERVED or (existing_relation is None and hasattr(model, name)):
                    raise TypeError(f"{ir['name']}.{name}: generated relation collides with an existing model member")
                relation = (
                    existing_relation if existing_relation is not None and existing_relation.ir() == r else _relation(r)
                )
                if relation is not existing_relation:
                    relation.__set_name__(model, name)
                relations[name] = relation
            for old in model._meta.relations.keys() - relations.keys():
                if old not in vars(model):
                    raise TypeError(
                        "class preparation cannot remove inherited members; use define() for this logical view"
                    )
            associations.append((model, ir, fields, relations))
        methods_by_model: dict[str, dict[str, Any]] = {}
        for method in prepared.get("behavior", {}).get("methods", ()):
            model = snapshot._models[method["model"]]
            if model._meta.registry is not self:
                continue
            name = method["name"]
            if name in _RESERVED or hasattr(model, name):
                raise TypeError(f"{method['model']}.{name}: generated method collides with an existing model member")
            methods_by_model.setdefault(method["model"], {})[name] = getattr(_native, method["native_function"])
        native = _native.Schema(prepared_json, classes, package_dirs() if prepared.get("behavior", {}).get("hooks") else None)
        snapshot._native = native
        read_flags: dict[str, dict[str, Any]] = json.loads(native.read_flags())
        snapshot._behavior = prepared.get("behavior", {})
        # Associations are published only after all native validation succeeds.
        for model, ir, fields, relations in associations:
            meta = model._meta
            for name in meta.fields.keys() - fields.keys():
                delattr(model, name)
            for name, field in fields.items():
                if meta.fields.get(name) is not field:
                    setattr(model, name, field)
            for name in meta.relations.keys() - relations.keys():
                delattr(model, name)
            for name, relation in relations.items():
                if meta.relations.get(name) is not relation:
                    setattr(model, name, relation)
            meta.relations = relations
            meta.fields = fields
            meta.table = ir["table"]
            meta.key = _key_fields(meta.name, fields, ir.get("primary_key"))
            meta.schema_ir = ir
            meta.filtered = read_flags[meta.name]["filtered"]
            meta.index_names = tuple(read_flags[meta.name]["indexes"])
            if isinstance(getattr(model, "indexes", None), (_Indexes, type(None))):
                setattr(model, "indexes", _Indexes(model, meta.index_names))
            behavior = prepared.get("behavior", {})
            meta.guards = tuple(
                dict.fromkeys(d["field"] for d in behavior.get("instance_guards", ()) if d["model"] == meta.name)
            )
            meta.upsert_drop = frozenset(
                d["field"]
                for d in behavior.get("write_assignments", ())
                if d["model"] == meta.name and d["upsert"] == "drop" and "upsert" in d["on"]
            )
            proxy: dict[str, Any] = next(
                (p for p in prepared.get("behavior", {}).get("proxy_models", ()) if p["model"] == meta.name), {}
            )
            meta.narrowed = frozenset(f["field"] for f in proxy.get("fields", ()) if f.get("non_null"))
            meta.field_names = tuple(fields)
            meta.index_nested()
            meta.attach_fields = (
                {name: fields[name] for name in multi_table[meta.name][0]} if meta.name in multi_table else None
            )
            computed = {f["field"] for f in snapshot._behavior.get("result_fields", ()) if f["model"] == meta.name}
            meta.input_fields = {k: v for k, v in fields.items() if k not in computed} if computed else fields
            for name, function in methods_by_model.get(meta.name, {}).items():
                setattr(model, name, staticmethod(function))
            meta.registry = snapshot
            if _reference_adapter is not None:
                _reference_adapter.install(model)
        self._native = native
        self._behavior = _detached(snapshot._behavior)
        return native

    def _candidate(self) -> Registry:
        candidate = Registry()
        candidate._models = self._models.copy()
        candidate._enums = self._enums.copy()
        candidate._enum_ir = self._enum_ir.copy()
        candidate._dialect = self._dialect
        candidate._schema_extra = _detached(self._schema_extra)
        candidate._behavior = _detached(self._behavior)
        candidate._identities = _detached(self._identities)
        return candidate

    def native(self) -> _native.Schema:
        if self._native is None:
            raise TypeError("registry changed; call registry.prepare() after declaring the complete model batch")
        return self._native


_RESERVED = {
    "pk",
    "objects",
    "qs",
    "_meta",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "update",
    "delete",
    "refresh",
    "load",
    "to_dict",
    "_field_value",
    "_orm_internal",
}

registry = Registry()


def _default_registry() -> Registry:
    return registry


def _multi_table(models: Sequence[dict[str, Any]], behavior: dict[str, Any]) -> dict[str, tuple[tuple[str, ...], bool]]:
    """Per child model of a shared-key chain: its own field names, and whether the root key fills itself."""
    parents = {link["child"]: link["parent"] for link in behavior.get("owner_links", ())}
    if not parents:
        return {}
    stored = {(s["model"], s["field"]): s["owner"] for s in behavior.get("field_storage", ())}
    tables = {p["table"]: p["name"] for p in (behavior.get("storage") or {}).get("models", ())}
    owner_of = {m["name"]: tables.get(m["table"], m["name"]) for m in models}
    by_owner = {owner_of[m["name"]]: m for m in reversed(models)}
    out: dict[str, tuple[tuple[str, ...], bool]] = {}
    for model in models:
        name = model["name"]
        owner = owner_of[name]
        if owner not in parents:
            continue
        local = tuple(
            f["name"] for f in model["fields"] if not f.get("primary_key") and stored.get((name, f["name"]), owner) == owner
        )
        root = owner
        while root in parents:
            root = parents[root]
        key: dict[str, Any] = next((f for f in by_owner[root]["fields"] if f.get("primary_key")), {}) if root in by_owner else {}
        filled = key.get("auto_increment", False) or "default" in key or key.get("default_now", False) or "default_sql" in key
        out[name] = (local, bool(filled))
    return out


def _key_default(multi_table: dict[str, tuple[tuple[str, ...], bool]], model: dict[str, Any], field: dict[str, Any]) -> bool:
    return bool(field.get("primary_key")) and multi_table.get(model["name"], ((), False))[1]


def _field(ir: dict[str, Any], nested_types: dict[str, NestedType] | None = None, *, key_default: bool = False) -> Field[Any]:
    kwargs: dict[str, Any] = {
        "primary_key": ir.get("primary_key", False),
        "auto_increment": ir.get("auto_increment", False),
        "nullable": ir.get("nullable", False),
        "unique": ir.get("unique", False),
        "index": ir.get("index", False),
        "column": ir["column"],
        "default_now": ir.get("default_now", False),
        "server_default": "default_sql" in ir or key_default,
    }
    if "default" in ir:
        kwargs["default"] = ir["default"]
    if "client_default" in ir:
        kwargs["client_default"] = ir["client_default"]
    nested = ir.get("nested")
    if nested is not None and nested["storage"] == "json" and nested_types is not None:
        return NestedJson(ir, nested_types[nested["type"]], **kwargs)
    if ir.get("array"):
        return Array(ir["type"], enum=ir.get("enum"), **kwargs)
    if "enum" in ir:
        return Enum(ir["enum"], stored=ir["type"], **kwargs)
    cls = BY_TYPE[ir["type"]]
    if cls is String:
        return String(ir.get("max_length"), **kwargs)
    return cls(**kwargs)


def _relation(ir: dict[str, Any]) -> Relation[Any, Any]:
    if ir["kind"] == "one" and ir.get("foreign_key"):
        return BelongsTo(ir["target"], via=ir["from"], to=ir["to"], on_delete=ir.get("on_delete", "no_action"))
    if ir["kind"] == "one":
        return HasOne(ir["target"], via=ir["to"], from_=ir["from"])
    if "through" in ir:
        t = ir["through"]
        return ManyToMany(
            ir["target"],
            through=t["model"],
            source=t["source"],
            target_field=t["target"],
            from_=ir["from"],
            to=ir["to"],
        )
    return HasMany(ir["target"], via=ir["to"], from_=ir["from"])


def _enum(ir: dict[str, Any], module: str | None) -> type[enum.Enum]:
    """The Python class of a schema enum: an ``IntEnum`` for int storage, otherwise a
    ``StrEnum`` whose values are the stored labels."""
    base: type[enum.Enum] = enum.IntEnum if ir.get("storage") == "int" else enum.StrEnum
    members = [(v["name"], v["value"]) for v in ir["values"]]
    cls: type[enum.Enum] = cast(Any, base)(ir["name"], members, module=module)
    return cls


_CLASS_BODY = {
    "__module__", "__qualname__", "__doc__", "__dict__", "__weakref__", "__annotations__", "__annotate__",
    "__annotate_func__", "__annotations_cache__", "__firstlineno__", "__static_attributes__", "__orig_bases__",
    "__parameters__", "__type_params__", "__slots__", "__abstractmethods__", "_abc_impl",
}


_DISPLAY = {"__str__", "__repr__"}


def _check_methods(model: str, methods: type, members: set[str]) -> None:
    for base in methods.__mro__[:-1]:
        for name in vars(base):
            if name in _CLASS_BODY:
                continue
            if name in members or name in _RESERVED or (hasattr(Model, name) and name not in _DISPLAY):
                raise TypeError(f"{model}.{name}: model method collision")


def define(
    schema: str | dict[str, Any],
    *,
    registry: Registry | None = None,
    module: str | None = None,
    required_capabilities: tuple[str, ...] = (),
    extensions: dict[str, types.ModuleType] | None = None,
    members: dict[str, tuple[str, ...]] | None = None,
    model_methods: dict[str, type] | None = None,
) -> dict[str, Any]:
    """Build model classes from a compiled schema (the JSON IR ``orm compile`` and
    :func:`load` produce). Generated ``models.py`` modules call this.

    Returns the model classes and the schema's enum classes, by name. They join
    ``registry`` (the default one unless given); ``module`` sets their ``__module__``.

    ``extensions`` maps an extension id to its runtime module, and ``members`` maps a model to the
    ids whose query-set and instance classes it gets, dependencies first. Generated modules pass both.
    ``model_methods`` maps a model to a class whose methods go on each of its instances (``@@model_methods``).
    """
    from .query import BoundQuerySet, QuerySet, use_extensions

    for capability in required_capabilities:
        if capability not in _CAPABILITIES:
            raise TypeError(f"generated models require {capability}; rebuild/select a compatible native artifact")
    destination = registry if registry is not None else _default_registry()
    context = json.dumps(destination.ir()) if destination._models else None
    if isinstance(schema, str):
        try:
            prepared_json = _native.prepare_schema(schema, context)
        except _native.SchemaError:
            json.loads(schema)  # malformed JSON keeps raising JSONDecodeError
            raise
    else:
        prepared_json = _native.prepare_schema(json.dumps(schema), context)
    ir: dict[str, Any] = json.loads(prepared_json)
    extensions = dict(extensions or {})
    packaged = _extensions.packages(ir, extensions)
    selected = _extensions.package_members(ir, packaged)
    selected.update(members or {})
    chains = {
        name: [(ident, packaged[ident] if ident in packaged else _extensions.check(ident, extensions[ident])) for ident in ids]
        for name, ids in selected.items()
    }
    reg = destination._candidate()
    nested_types: dict[str, NestedType] = {}
    dialect = ir.get("dialect", "postgres")
    if reg._dialect is not None and reg._dialect != dialect:
        raise TypeError("schemas in one registry must target the same database")
    reg._dialect = dialect
    out: dict[str, Any] = {}
    for e in ir.get("enums", ()):
        cls_e = reg._enums.get(e["name"]) if reg._enum_ir.get(e["name"]) == e else None
        cls_e = cls_e or _enum(e, module)
        reg.register_enum(e, cls_e)
        out[e["name"]] = cls_e
    if ir.get("types"):
        nested_types = build_types(ir["types"], reg._enums, module)
        out.update({n: t.cls for n, t in nested_types.items()})
    multi_table = _multi_table(ir["models"], ir.get("behavior", {}))
    for m in ir["models"]:
        if m["name"] in destination._models:
            if m != destination._models[m["name"]]._meta.ir():
                raise TypeError(
                    f"extension changed existing model {m['name']}; define dependent schemas together in a new registry"
                )
            continue
        ns: dict[str, Any] = {
            f["name"]: _field(f, nested_types, key_default=_key_default(multi_table, m, f)) for f in m["fields"]
        }
        for group, leaves in leaf_names(m).items():
            if group in ns:
                raise TypeError(f"{m['name']}.{group}: nested field collides with a field")
            first = next(f["nested"] for f in m["fields"] if f["name"] == leaves[0][0])
            leaf_irs = [f["nested"] for f in m["fields"] if f["name"] in dict(leaves)]
            under = not first.get("root_required", False) and all(n.get("under_null", False) for n in leaf_irs)
            ns[group] = NestedGroup(group, nested_types[first["type"]], under, tuple(leaves))
        ns.update({r["name"]: _relation(r) for r in m.get("relations", ())})
        if module is not None:
            ns["__module__"] = module
        chain = chains.get(m["name"], [])
        classes = (
            [(getattr(extensions[i], r["query_set"]), getattr(extensions[i], r["instance"])) for i, r in chain]
            if chain
            else []
        )
        if chain:
            _extensions.members(
                m["name"],
                set(ns),
                chain,
                lambda n: hasattr(QuerySet, n),
                lambda n: hasattr(Model, n) or n in _RESERVED,
                lambda n: hasattr(BoundQuerySet, n),
            )
        methods = (model_methods or {}).get(m["name"])
        if methods is not None:
            _check_methods(m["name"], methods, set(ns).union(*(r["instance_members"] for _, r in chain)))
        cls: type[Model] = types.new_class(
            m["name"],
            (*(c[1] for c in reversed(classes)), *((methods,) if methods is not None else ()), Model),
            {"table": m["table"], "registry": reg, "key": m.get("primary_key")},
            lambda body: body.update(ns),
        )
        if classes:
            bound = tuple(
                getattr(extensions[i], r["bound_query_set"])
                for i, r in reversed(chain)
                if r["bound_query_set"]
            )
            use_extensions(cls, tuple(c[0] for c in reversed(classes)), bound)
        for method in ir.get("behavior", {}).get("methods", ()):
            if method["model"] == m["name"]:
                if method["name"] in ns or method["name"] in _RESERVED:
                    raise TypeError(f"{m['name']}.{method['name']}: model method collision")
        cls._meta.schema_ir = m
        cls._meta.key = tuple(cls._meta.fields[f.name] for f in cls._meta.key)
        computed = {f["field"] for f in ir.get("behavior", {}).get("result_fields", ()) if f["model"] == m["name"]}
        if computed:
            cls._meta.input_fields = {k: v for k, v in cls._meta.fields.items() if k not in computed}
        out[m["name"]] = cls
    for key in ("types", "extensions", "functions", "catalog", "partitions", "native_expression_slots"):
        items = reg._schema_extra.setdefault(key, [])
        items.extend(x for x in ir.get(key, ()) if x not in items)
        if not items:
            del reg._schema_extra[key]
    reg._behavior = _detached(ir.get("behavior", {}))
    reg._identities = _detached(ir.get("identities"))
    reg._prepare(None if _REPREPARE else (prepared_json, ir))
    destination._models = reg._models.copy()
    destination._enums = reg._enums.copy()
    destination._enum_ir = reg._enum_ir.copy()
    destination._dialect = reg._dialect
    destination._schema_extra = {k: v.copy() for k, v in reg._schema_extra.items()}
    destination._native = reg._native
    destination._behavior = _detached(reg._behavior)
    destination._identities = _detached(reg._identities)
    return out


def load(
    path: str | PathLike[str],
    *,
    registry: Registry | None = None,
    module: str | None = None,
    model_methods: dict[str, type] | None = None,
) -> dict[str, Any]:
    """Compile a schema file (``schema.prisma``) and build its models: no generated code
    needed (generate ``models.py`` / ``.pyi`` for editor and type-checker support).
    An ``import "app:<name>"`` resolves to the installed app from its package metadata; no app code is imported.
    ``model_methods`` is as for :func:`define`."""
    return define(
        _native.compile_schema_file(str(path), resolve_apps(path), resolve_extensions()), registry=registry, module=module, model_methods=model_methods
    )


class Model:
    """Base class of generated model classes.

    Instances keep column values in their ``__dict__``. ``Model.objects`` is the root
    :class:`~orm.query.QuerySet` of the model.
    """

    _meta: ClassVar[ModelMeta]
    objects: ClassVar[QuerySet[Any]]
    DoesNotExist: ClassVar[type[DoesNotExist]] = DoesNotExist
    MultipleObjectsReturned: ClassVar[type[MultipleObjectsReturned]] = MultipleObjectsReturned

    def __init_subclass__(
        cls,
        *,
        table: str | None = None,
        registry: Registry | None = None,
        key: Sequence[str] | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init_subclass__(**kwargs)
        from .query import QuerySet

        reg = registry if registry is not None else _default_registry()
        cls._meta = ModelMeta(cls, table or cls.__name__.lower(), reg, key)
        cls.objects = QuerySet(cls)
        for base in (DoesNotExist, MultipleObjectsReturned):
            ns = {"__qualname__": f"{cls.__qualname__}.{base.__name__}", "__module__": cls.__module__}
            setattr(cls, base.__name__, type(base.__name__, (base,), ns))
        reg.register(cls)

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        name = type(self).__name__
        raise TypeError(
            f"{name} instances come from the database; insert a row with `await {name}.objects.insert(...)`"
        )

    def __setattr__(self, name: str, value: Any) -> None:
        raise AttributeError(
            f"{type(self).__name__} instances are read-only snapshots of a row; "
            f"write with `await obj.update({name}=...)`"
        )

    def __delattr__(self, name: str) -> None:
        raise AttributeError(f"{type(self).__name__} instances are read-only")

    @property
    def pk(self) -> Any:
        """The primary key value; a tuple in key order for a key of several fields."""
        data, internal = self.__dict__, self.__dict__.get("_orm_internal", {})
        values = tuple(data.get(f.name, internal.get(f.name)) for f in self._meta.key)
        if len(values) == 1:
            return values[0]
        return None if any(v is None for v in values) else values

    def _field_value(self, name: str) -> Any:
        """Internal key access for relation loaders; never marks a field loaded."""
        if name in self.__dict__:
            return self.__dict__[name]
        internal = self.__dict__.get("_orm_internal", {})
        if name in internal:
            return internal[name]
        raise NotLoaded(f"{type(self).__name__}.{name} was not loaded")

    def to_dict(self) -> dict[str, Any]:
        """Serialize public loaded scalar fields, retaining loaded NULL values."""
        return {n: self.__dict__[n] for n in self._meta.public_names if n in self.__dict__}

    def qs(self) -> BoundQuerySet[Self]:
        """A query set bound to this object: ``type(self).objects.of(self)``."""
        return type(self).objects.of(self)

    async def load(self, *items: ColumnRef[Any] | RelationPath[Any] | QuerySet[Any], reload: bool = False) -> Self:
        """Populate requested fields and relations, preserving loaded values by default.

        Accepts the same arguments as QuerySet.load(). No arguments loads all scalar
        fields. Returns this instance; reload=True refreshes the requested values.
        """
        from ._loading import load_instance

        await load_instance(self, items, reload)
        return self

    # -- writes --------------------------------------------------------------------------
    # Each method is one statement on this row (matched by primary key), run on the
    # database the instance was read from.

    def _bound(self, *extra: str) -> BoundQuerySet[Self]:
        """This row as a bound set. It applies no default on the row, so a default cannot hide it
        from its own write or load; a relation or aggregate keeps the default of its target.
        A partial instance keeps its public shape plus ``extra``."""
        bound = self.qs()
        if "_orm_internal" not in self.__dict__:
            return bound
        names = [n for n in self._meta.public_names if n in self.__dict__]
        names.extend(n for n in extra if n not in names)
        return bound.load(*(ColumnRef(type(self), (), self._meta.fields[n]) for n in self._meta.expand(names)))

    def _versioned(self, bound: BoundQuerySet[Self]) -> BoundQuerySet[Self]:
        """With instance guards, match only the loaded values of the row. The set is marked as guarded,
        so a bound-write extension does not add the same check."""
        for name in self._meta.guards:
            bound = bound.filter(ColumnRef(type(self), (), self._meta.fields[name]) == getattr(self, name))
        return bound._guard() if self._meta.guards else bound

    async def _raise_if_changed(self) -> None:
        """No row matched a guarded write: raise when the row has other guard values."""
        for name in self._meta.guards:
            changed = self.qs().filter(differs(ColumnRef(type(self), (), self._meta.fields[name]), getattr(self, name)))
            if await changed.exists():
                raise VersionConflict(f"{type(self).__name__} {self.pk!r} was changed since it was loaded")

    async def update(self, **values: Any) -> None:
        """``UPDATE ... SET <values> WHERE pk = ... RETURNING`` the loaded fields.

        Values may be expressions (``views=Post.views + 1``); the instance is refreshed
        from the returned row, so it shows what the database stored. With an
        instance guard field, raises :class:`orm.VersionConflict` when the row has a
        newer value. An explicit guard field raises :class:`TypeError`; use a
        query-set update for an intentional override.
        """
        if not values:
            return
        for guard in self._meta.guards:
            if guard in values:
                raise TypeError(
                    f"{type(self).__name__}.update() cannot set the version field {guard}; "
                    f"use {type(self).__name__}.objects.filter(pk=...).update({guard}=...) to override it"
                )
        bound = self._versioned(self._bound(*self._meta.guards))
        if not await bound.update(**values):
            await self._raise_if_changed()
            raise self.DoesNotExist(f"{type(self).__name__} {self.pk!r} no longer exists")

    async def delete(self) -> None:
        """``DELETE ... WHERE pk = ... RETURNING`` the loaded fields; the instance is filled from
        the returned row, so a delete rewrite shows its stored value. With an instance guard field, raises :class:`orm.VersionConflict`
        when the row has a newer value; a hard delete has no guard check."""
        bound = self._versioned(self._bound(*self._meta.guards))
        if not await bound.delete():
            await self._raise_if_changed()

    # -- dunder --------------------------------------------------------------------------

    def __eq__(self, other: object) -> bool:
        if type(other) is not type(self):
            return NotImplemented
        pk = self.pk
        return pk is not None and pk == other.pk

    def __hash__(self) -> int:
        if self.pk is None:
            raise TypeError("model instances without a primary key value are unhashable")
        return hash((type(self), self.pk))

    def __repr__(self) -> str:
        d = self.__dict__
        shown = ", ".join(f"{n}={d[n]!r}" for n in self._meta.public_names if n in d)
        return f"{type(self).__name__}({shown})"


_PYTHON_TYPES: dict[str, type] = {
    "big_int": int,
    "int": int,
    "float": float,
    "bool": bool,
    "string": str,
    "text": str,
    "date_time": datetime.datetime,
    "date": datetime.date,
    "uuid": uuid.UUID,
    "json": object,
    "decimal": decimal.Decimal,
}
_RELATION_KINDS: tuple[tuple[type, str], ...] = (
    (ManyToMany, "many_to_many"),
    (HasMany, "has_many"),
    (HasOne, "has_one"),
    (BelongsTo, "belongs_to"),
)


def column(model: type[Model], path: str) -> ColumnRef[Any]:
    """The column a dotted path names: ``column(Bundle, "items.product.title")`` is
    ``Bundle.items.product.title``. Each name before the last is a relation (to-one or
    to-many), the last a field. For adapters that take names from a request, such as a
    search or ordering filter."""
    *hops, name = path.split(".")
    target: type[Model] = model
    for hop in hops:
        rel = target._meta.relations.get(hop)
        if rel is None:
            raise LookupError(f"{target.__name__} has no relation {hop!r} (in {path!r})")
        target = rel.target
    field = target._meta.fields.get(name)
    if field is None:
        raise LookupError(f"{target.__name__} has no field {name!r} (in {path!r})")
    return ColumnRef(model, tuple(hops), field)


def describe(model: type[Model]) -> dict[str, Any]:
    """Plain data about ``model`` for code that builds rows from it, such as a test
    factory: its fields (schema and Python type, nullability, enum, length, unique,
    who gives a default, whether ``insert()`` takes it), its relations and its unique
    keys. ``primary_key`` and relation ``from``/``to`` are lists in key order,
    even for a single field. Each entry is a new object; changing it changes nothing."""
    meta = model._meta
    ir = meta.ir()
    registry = meta.registry
    fields = []
    for f in ir["fields"]:
        enum_cls = registry.get_enum(f["enum"]) if "enum" in f else None
        python_type: type = enum_cls or _PYTHON_TYPES[f["type"]]
        database = bool(f.get("auto_increment") or "default" in f or f.get("default_now") or "default_sql" in f)
        fields.append(
            {
                "name": f["name"],
                "column": f["column"],
                "type": f["type"],
                "python_type": list if f.get("array") else python_type,
                "element_type": python_type if f.get("array") else None,
                "nullable": bool(f.get("nullable")),
                "array": bool(f.get("array")),
                "enum": enum_cls,
                "max_length": f.get("max_length"),
                "primary_key": bool(f.get("primary_key")),
                "unique": bool(f.get("unique")) or (f["name"],) == meta.key_names,
                "default": "database" if database else "client" if "client_default" in f else None,
                "insert": f["name"] in meta.input_fields,
            }
        )
    relations = []
    for name, rel in meta.relations.items():
        kind = next(k for cls, k in _RELATION_KINDS if isinstance(rel, cls))
        r = next(r for r in ir.get("relations", ()) if r["name"] == name)
        relations.append(
            {
                "name": name,
                "kind": kind,
                "target": rel.target,
                "from": [r["from"]] if isinstance(r["from"], str) else list(r["from"]),
                "to": [r["to"]] if isinstance(r["to"], str) else list(r["to"]),
                "through": registry.get(r["through"]["model"]) if "through" in r else None,
                "nullable": kind != "belongs_to" or any(meta.fields[n].nullable for n in rel.via_fields),  # type: ignore[attr-defined]
            }
        )
    unique: list[tuple[str, ...]] = [meta.key_names]
    unique += [(f["name"],) for f in ir["fields"] if f.get("unique") and (f["name"],) != meta.key_names]
    unique += [tuple(c["fields"]) for c in ir.get("constraints", ()) if c.get("kind") == "unique"]
    return {
        "name": meta.name,
        "table": meta.table,
        "primary_key": list(meta.key_names),
        "fields": fields,
        "relations": relations,
        "unique": unique,
    }


def loads(
    source: str,
    *,
    registry: Registry | None = None,
    module: str | None = None,
    model_methods: dict[str, type] | None = None,
) -> dict[str, Any]:
    """Like :func:`load`, for schema source text (``import`` paths resolve against the
    current directory). ``import "app:<name>"`` needs a file: use :func:`load` or generate the models."""
    return define(_native.compile_schema(source, None, resolve_extensions()), registry=registry, module=module, model_methods=model_methods)
