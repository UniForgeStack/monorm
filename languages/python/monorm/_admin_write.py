"""Host-side writes of the admin: the default writer, the service wrappers, ``Context`` and ``admin.log``.

The Rust service calls these functions with no transaction. A function owns its transactions
(``async with db.transaction()``) and answers with an envelope that the service maps to a status:
``{"ok": True, "key": ..., "audited": bool}``, or ``{"ok": False, "kind": "not_found" | "conflict" | "invalid" | "sync_service"}``.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import contextvars
import datetime
import decimal
import inspect
import re
import uuid
from collections.abc import Awaitable, Callable, Mapping
from typing import Any, TypeVar

from .adapters import _ADAPTERS as _adapters
from .adapters import FieldAdapterError
from .errors import DoesNotExist, ValidationError, VersionConflict
from .expr import Condition, IRContext, SlotParams, _Slot, resolve_null_rule
from .protection import allow_writes

F = TypeVar("F", bound=Callable[..., Any])

_THREADED = "_monorm_admin_threaded"


def threaded(fn: F) -> F:
    """Marks a plain (sync) service function. It runs in a worker thread, and its async database calls go through ``ctx.run(...)``.
    A create gets ``(ctx, values)`` with plain values. An update gets ``(ctx, qs, values)`` and a delete ``(ctx, qs)``, where ``qs`` is a query set.
    Use it as a decorator."""
    setattr(fn, _THREADED, True)
    return fn


def is_threaded(fn: Any) -> bool:
    return bool(getattr(fn, _THREADED, False))


def is_sync(fn: Any) -> bool:
    return not (inspect.iscoroutinefunction(fn) or inspect.iscoroutinefunction(getattr(fn, "__call__", None)))


class InputDef:
    """The definition of one input control; see :func:`input`."""

    def __init__(self, name: str, type: str, label: str | None, required: bool, help: str | None, choices: list[str], secret: bool = False) -> None:
        self.name = name
        self.type = type
        self.label = label
        self.required = required
        self.help = help
        self.choices = choices
        self.secret = secret

    def json(self) -> dict[str, Any]:
        out: dict[str, Any] = {"name": self.name, "type": self.type, "required": self.required, "choices": self.choices}
        if self.label is not None:
            out["label"] = self.label
        if self.help is not None:
            out["help"] = self.help
        if self.secret:
            out["secret"] = True
        return out


def input(
    name: str,
    *,
    type: str = "text",
    label: str | None = None,
    required: bool = False,
    help: str | None = None,
    choices: list[str] | tuple[str, ...] = (),
    secret: bool = False,
) -> InputDef:
    """A control of the form that is no field: ``admin.input("reason", type="textarea", required=True)``.

    ``type`` is ``text``, ``textarea``, ``number``, ``checkbox`` or ``select`` (``select`` needs ``choices``).
    The value lives for one request and only the service reads it as ``ctx.inputs[name]``.
    A resource with an input needs a service.
    With ``secret=True`` the audit row stores a mask (``***``) in place of the value.
    """
    return InputDef(name, type, label, required, help, list(choices), secret)


_PLAIN_USER_DROP = ("password_hash",)


class Context:
    """What a service function knows about the request.

    ``user`` is the plain user of the admin, ``request_id`` the id of the request, ``loaded`` the values that the form showed
    (by field name; empty on a create), ``inputs`` the values of the ``admin.input`` controls and ``inlines`` the change set of the
    inline rows that the form sent with the record (see below). ``database`` is the database
    of the request. A unit test builds a ``Context`` by hand and calls the service function: no admin is needed.

    ``inlines`` maps an inline name to ``{"model", "pin", "create", "update", "delete"}``: ``create`` is a list of value dicts,
    ``update`` a list of ``{"key", "set", "loaded", "before"}``, ``delete`` a list of ``{"key", "before"}``. ``pin`` pairs each
    child field with the parent field it holds. The admin has checked every row; the service writes them in its own transaction,
    deletes first, then updates, then creates. A service that ignores ``ctx.inlines`` drops the inline rows.

    An action also reads ``count`` (the rows of its query set, at most 10,001), ``capped`` (``True`` when the count hit the cap) and
    ``selection`` (``"keys"`` for the rows that staff picked, ``"all"`` for every row that matches, ``"filter"`` for a list action).
    """

    def __init__(
        self,
        *,
        user: Mapping[str, Any] | None = None,
        request_id: str = "",
        loaded: Mapping[str, Any] | None = None,
        inputs: Mapping[str, Any] | None = None,
        audit_inputs: Mapping[str, Any] | None = None,
        database: Any = None,
        key: Mapping[str, Any] | None = None,
        resource: str = "",
        op: str = "",
        ip: str | None = None,
        user_agent: str | None = None,
        inlines: Mapping[str, Any] | None = None,
        count: int = 0,
        capped: bool = False,
        selection: str = "keys",
    ) -> None:
        self.count = count
        self.capped = capped
        self.selection = selection
        self.inlines: dict[str, Any] = dict(inlines or {})
        self.user: dict[str, Any] = {k: v for k, v in (user or {}).items() if k not in _PLAIN_USER_DROP}
        self.request_id = request_id
        self.loaded: dict[str, Any] = dict(loaded or {})
        self.inputs: dict[str, Any] = dict(inputs or {})
        self._audit_inputs: dict[str, Any] = dict(self.inputs if audit_inputs is None else audit_inputs)
        self.database = database
        self.key: dict[str, Any] | None = None if key is None else dict(key)
        self.resource = resource
        self.op = op
        self.ip = ip
        self.user_agent = user_agent
        self.audits: list[dict[str, Any]] = []
        self._audit_keys: list[dict[str, Any]] = []
        self._audited = False
        self._by_admin = False
        self._loop: asyncio.AbstractEventLoop | None = None
        self._model: Any = None

    def run(self, awaitable: Awaitable[Any]) -> Any:
        """Runs an awaitable on the event loop and waits for the result. For a ``threaded`` sync service only:
        its worker thread calls the async database API through here."""
        if self._loop is None:
            raise RuntimeError("ctx.run() works inside a threaded service that the admin runs")
        return asyncio.run_coroutine_threadsafe(_as_coroutine(awaitable), self._loop).result()

    async def audit(self, record: Any = None, *, action: str | None = None, changes: Mapping[str, Any] | None = None) -> None:
        """Writes the audit row of this write on the database of the request (in the transaction that is open now).

        The row holds the user, the action (the operation by default), the key of the record, the inputs (a secret input as a mask) and ``changes``.
        A create passes the new record (an instance, a key value or a mapping of the key) so that the row names it.
        With this call the admin writes no audit row of its own for the request.
        """
        key = self.key
        model = self._model if self._model is not None else type(record) if hasattr(type(record), "_meta") else None
        if record is not None and model is not None:
            key = _key_of(model, record)
        if key is None and self.op == "create":
            raise ValueError("ctx.audit(record) on a create needs the new record: the row names it")
        extra: dict[str, Any] = {"request_id": self.request_id, "inputs": dict(self._audit_inputs), "service": True}
        if self.ip is not None:
            extra["ip"] = self.ip
        if self.user_agent is not None:
            extra["user_agent"] = self.user_agent
        if changes:
            extra.update(changes)
        act = action or self.op or "service"
        resource = self.resource or (model.__name__ if model is not None else "")
        parts = None if not key else [_key_scalar(v) for v in key.values()]
        entry: dict[str, Any] = {"action": act, "resource": resource, "key": parts, "changes": extra}
        self.audits.append(entry)
        self._audited = True
        if self.database is not None:
            # Under the admin the row keeps a blank key text: Rust writes it from `audit_keys`, the only place that encodes a key.
            # A context built by hand (a unit test) has no Rust: its row holds the plain key values joined with `,`.
            text = "" if self._by_admin or not parts else ",".join(parts)
            row = await _write_log(self.database, _user_key(self.user), act, resource, text, "", extra)
            self._audit_keys.append({"id": int(row.id), "key": parts})


async def _as_coroutine(awaitable: Awaitable[Any]) -> Any:
    return await awaitable


def _user_key(user: Mapping[str, Any]) -> str:
    key = user.get("key")
    return "" if key is None else str(key)


def _key_scalar(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (datetime.date, datetime.time)):
        return value.isoformat()
    return str(value)


def _key_of(model: Any, record: Any) -> dict[str, Any]:
    names = model._meta.key_names
    if isinstance(record, Mapping):
        return {n: record[n] for n in names}
    if isinstance(record, model):
        return {n: getattr(record, n) for n in names}
    if len(names) == 1:
        return {names[0]: record}
    if isinstance(record, (list, tuple)) and len(record) == len(names):
        return dict(zip(names, record, strict=True))
    raise TypeError(f"{model.__name__}: cannot read a key from {record!r}")


def _actor_key(user: Any) -> Any:
    """The key of a user: its ``key``, else its ``id`` (a mapping or an object); anything else is the key itself."""
    if isinstance(user, Mapping):
        return user.get("id") if user.get("key") is None else user["key"]
    if user is None or isinstance(user, (str, int)):
        return user
    key = getattr(user, "key", None)
    return getattr(user, "id", None) if key is None else key


async def log(
    action: str,
    *,
    user: Any = None,
    resource: str = "",
    record_key: Any = "",
    label: str = "",
    changes: Mapping[str, Any] | None = None,
    using: Any = None,
) -> None:
    """Writes one row of the audit log from host code outside the admin: a job, a script, a webhook.

    ``user`` is the user of the row (a user mapping or object: its ``key``, else its ``id``; a user key; or None), ``record_key`` the key of the record
    (text, or a key value). The row goes through the database ``using`` (the default database) and joins the
    transaction that is open. It needs the ``AdminLogEntry`` model of the admin app in the registry.
    """
    from .db import resolve

    db = resolve(using)
    key = _actor_key(user)
    await _write_log(db, "" if key is None else str(key), action, resource, record_key if isinstance(record_key, str) else _key_scalar(record_key), label, dict(changes) if changes else None)


async def _write_log(db: Any, user_key: str, action: str, resource: str, record_key: str, label: str, changes: Mapping[str, Any] | None) -> Any:
    registry = db._registry
    try:
        entry_model = registry.get("AdminLogEntry")
    except Exception:
        raise RuntimeError("the admin log needs the AdminLogEntry model: import the admin app schema") from None
    return await entry_model.objects.using(db).insert(
        user_key=user_key, action=action, resource=resource, record_key=record_key, record_label=label, changes=None if changes is None else dict(changes)
    )


# -- values from the JSON of the request -----------------------------------------------------------------------------


def _decode(type_name: str, value: Any) -> Any:
    if value is None:
        return None
    if type_name == "date_time" and isinstance(value, str):
        parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed if parsed.tzinfo else parsed.replace(tzinfo=datetime.UTC)
    if type_name == "date" and isinstance(value, str):
        return datetime.date.fromisoformat(value)
    if type_name == "time" and isinstance(value, str):
        return datetime.time.fromisoformat(value)
    if type_name == "big_int" and isinstance(value, str) and re.fullmatch(r"-?\d+", value):
        return int(value)
    if type_name == "bytes" and isinstance(value, str):
        return base64.b64decode(value, validate=True)
    return value


def _field_type(field: Any) -> str:
    return str(getattr(field, "of", None) or field.type_name)


def decode_values(model: Any, values: Mapping[str, Any]) -> dict[str, Any]:
    """The values of a request (JSON, by field name) as Python values of the fields."""
    fields = model._meta.fields
    out: dict[str, Any] = {}
    for name, value in values.items():
        field = fields.get(name)
        if field is None:
            out[name] = value
        elif hasattr(field, "of") and isinstance(value, list):
            out[name] = [_decode(_field_type(field), v) for v in value]
        else:
            out[name] = _decode(field.type_name, value)
    return out


# -- the filters of the resource -------------------------------------------------------------------------------------


class _Bound(Condition):
    """The filter of a prepared ``queryset``, with the user key put into its ``param("user")``."""

    __slots__ = ("conditions", "user_key")

    def __init__(self, conditions: list[Condition], user_key: Any) -> None:
        self.conditions = conditions
        self.user_key = user_key

    def _ir(self, ctx: IRContext) -> Any:
        inner = SlotParams()
        irs = [resolve_null_rule(c)._ir(IRContext(ctx.root, inner)) for c in self.conditions]
        ir = irs[0] if len(irs) == 1 else {"t": "and", "items": irs}
        return self._remap(ir, inner, ctx.params)

    def _remap(self, ir: Any, inner: list[Any], params: list[Any]) -> Any:
        if isinstance(ir, list):
            return [self._remap(i, inner, params) for i in ir]
        if not isinstance(ir, dict):
            return ir
        if ir.get("t") == "param":
            value = inner[ir["i"]]
            if isinstance(value, _Slot):
                if value.name != "user":
                    raise ValueError(f"the queryset takes the param 'user', not {value.name!r}")
                value = self.user_key
            params.append(value)
            return {**ir, "i": len(params) - 1}
        return {k: self._remap(v, inner, params) for k, v in ir.items()}

    def __repr__(self) -> str:
        return f"queryset({self.conditions!r})"


async def _resource_filters(config: Mapping[str, Any], user: Mapping[str, Any], op: str) -> list[Any]:
    """The conditions the admin applies to one write: the ``queryset`` of the user and the ``can=`` rule of the operation."""
    from .expr import Expression
    from .query import Prepared

    out: list[Any] = []
    scope = config.get("queryset")
    if isinstance(scope, Prepared):
        out.append(_Bound(list(scope._qs._filters), user.get("key")))
    elif scope is not None:
        found = await scope(dict(user))
        out.extend([found] if isinstance(found, (Condition, Expression)) else list(found or ()))
    rule = config.get("can", {}).get({"update": "change", "delete": "delete"}.get(op, ""))
    if rule is not None:
        out.append(rule)
    return out


def _one_row(model: Any, db: Any, key: Mapping[str, Any], filters: list[Any]) -> Any:
    fields = model._meta.fields
    qs = model.objects.using(db).filter(*[getattr(model, name) == _decode(fields[name].type_name, value) for name, value in key.items()])
    return qs.filter(*filters) if filters else qs


# -- the envelope ----------------------------------------------------------------------------------------------------


def _key_json(model: Any, record: Any) -> dict[str, Any]:
    from .admin import _key_value

    return {name: _key_value(value) for name, value in _key_of(model, record).items()}


def _invalid(e: ValidationError) -> dict[str, Any]:
    return {"ok": False, "kind": "invalid", "fields": e.fields, "form": e.non_field_messages}


def _decode_inlines(raw: Mapping[str, Any] | None, models: Mapping[str, Any]) -> dict[str, Any]:
    """The change set of the inlines with the values of each row decoded to the types of the child model."""
    out: dict[str, Any] = {}
    for name, spec in (raw or {}).items():
        child = models[spec["model"]]
        out[name] = {
            "model": child,
            "pin": spec["pin"],
            "create": [decode_values(child, r["set"]) for r in spec["create"]],
            "update": [{"key": r["key"], "set": decode_values(child, r["set"]), "loaded": decode_values(child, r["loaded"]), "before": decode_values(child, r["before"])} for r in spec["update"]],
            "delete": [{"key": r["key"], "before": decode_values(child, r["before"])} for r in spec["delete"]],
        }
    return out


def _context(database: Any, args: Mapping[str, Any], model: Any, models: Mapping[str, Any] | None = None) -> Context:
    ctx = Context(
        user=args.get("user"),
        request_id=args.get("request_id") or "",
        loaded=decode_values(model, args.get("loaded") or {}),
        inputs=args.get("inputs") or {},
        audit_inputs=args.get("audit_inputs"),
        database=database,
        key=args.get("key"),
        resource=args.get("resource") or model.__name__,
        op=args.get("op") or "",
        ip=args.get("ip"),
        user_agent=args.get("user_agent"),
        inlines=_decode_inlines(args.get("inlines"), models or {}),
        count=int(args.get("count") or 0),
        capped=bool(args.get("capped")),
        selection=args.get("selection") or "keys",
    )
    ctx._model = model
    ctx._by_admin = True
    return ctx


def _same(type_name: str, stored: Any, loaded: Any) -> bool:
    """The compare of the Rust core (`json::same`): a decimal compares by number, so `5` and `5.00` match."""
    if stored == loaded:
        return True
    if stored is None or loaded is None:
        return False
    if type_name == "decimal":
        try:
            return bool(decimal.Decimal(str(stored)) == decimal.Decimal(str(loaded)))
        except decimal.InvalidOperation:
            return False
    return str(stored) == str(loaded)


async def _check_loaded(model: Any, qs: Any, args: Mapping[str, Any]) -> bool:
    """Reads the row (under a lock where the database has one) and compares the loaded values of the ``compare`` fields with it. False when the row is gone.

    The compare runs in the write transaction, so a concurrent edit between the load and the save is a ``VersionConflict``."""
    names = [n for n in args.get("compare") or [] if n in (args.get("loaded") or {})]
    row = await (qs.lock() if args.get("lock") else qs).first()
    if row is None:
        return False
    loaded = decode_values(model, {n: args["loaded"][n] for n in names})
    fields = model._meta.fields
    adapters = _adapters.get(model) or {}
    for n in names:
        # An adapted field shows a decoded value; the form loaded its stored bytes (base64), so compare those.
        current = getattr(row, n)
        if n in adapters and (stored := _stored_bytes(adapters[n], current)) is not None:
            current = stored
            if isinstance(loaded[n], str):
                loaded[n] = base64.b64decode(loaded[n], validate=True)
        if not _same(fields[n].type_name if n in fields else "", current, loaded[n]):
            raise VersionConflict(f"{model.__name__} {args.get('key')!r} was changed since the form loaded")
    return True


def _stored_bytes(adapter: Any, value: Any) -> bytes | None:
    """The stored bytes of a decoded adapter value, or None when the adapter stores something else."""
    try:
        stored = adapter.validate(value)
    except FieldAdapterError:
        return None
    return bytes(stored) if isinstance(stored, (bytes, bytearray, memoryview)) else None


async def _write_audit(database: Any, model: Any, args: Mapping[str, Any]) -> int:
    """The audit row of a default write, inside the write transaction. Answers its id.

    Rust sends the key text of an update and a delete (its own encoder); the admin sets the label, the key text of a create and the diff after it reads the record, by the id that this function answers."""
    changes: dict[str, Any] = {"request_id": args.get("request_id") or "", "inputs": dict(args.get("audit_inputs") or {}), "service": False}
    for name in ("ip", "user_agent"):
        if args.get(name) is not None:
            changes[name] = args[name]
    entry = await _write_log(database, _user_key(args.get("user") or {}), args["op"], args.get("resource") or model.__name__, args.get("key_text") or "", "", changes)
    return int(entry.id)


class _RowFailed(Exception):
    """A row of an ``update_rows`` call answered a failure: it ends the loop and rolls the transaction back."""

    def __init__(self, index: int, envelope: dict[str, Any]) -> None:
        self.index = index
        self.envelope = envelope


def per_row(fn: Callable[[Any, dict[str, Any]], Awaitable[Any]]) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """Lets a host write function answer an ``update_rows`` call: one transaction, ``fn`` once per row in the order of ``rows`` (key order).

    The first failure ends the loop and rolls back every row; the answer names the row by its ``index``.
    A failure envelope that returns inside ``transaction()`` would commit, so the loop raises to roll back."""

    async def call(database: Any, args: dict[str, Any]) -> Any:
        if args.get("op") != "update_rows":
            return await fn(database, args)
        results: list[Any] = []
        try:
            async with database.transaction():
                for index, row in enumerate(args["rows"]):
                    out = await fn(database, row)
                    if not out.get("ok"):
                        raise _RowFailed(index, out)
                    results.append(out)
        except _RowFailed as failed:
            return {**failed.envelope, "index": failed.index}
        return {"ok": True, "results": results}

    return call


def default_writer(model: Any, config: Mapping[str, Any]) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function ``admin.write``'s share for one model: a query-set write with the filters of the resource.

    It runs in one transaction: the compare of the loaded values, the write and the audit row commit or roll back together."""

    async def write(database: Any, args: dict[str, Any]) -> Any:
        op = args["op"]
        bulk = "filters" in args
        # A bulk delete sends a query set: `values` are the binds of its filters, not field values.
        values = {} if bulk else decode_values(model, args.get("values") or {})
        user = args.get("user") or {}
        protect = allow_writes(model) if args.get("allow_protected") else contextlib.nullcontext()
        try:
            with protect:
                async with database.transaction():
                    if bulk:
                        # The admin writes the summary row of a bulk delete.
                        n = int(await _query_set(model, database, args).delete() or 0)
                        return {"ok": True, "audited": False, "affected": n, "result": {"kind": "message", "message": f"Deleted {n} row(s)."}}
                    if op == "create":
                        created = await model.objects.using(database).insert(**values)
                        return {"ok": True, "key": _key_json(model, created), "audited": True, "audit_id": await _write_audit(database, model, args)}
                    qs = _one_row(model, database, args["key"], await _resource_filters(config, user, op))
                    if not await _check_loaded(model, qs, args):
                        return {"ok": False, "kind": "not_found"}
                    if op == "update":
                        done = await qs.update(**values) if values else await qs.exists()
                    else:
                        done = await qs.delete()
                    if not done:
                        return {"ok": False, "kind": "not_found"}
                    return {"ok": True, "key": args["key"], "audited": True, "audit_id": await _write_audit(database, model, args)}
        except ValidationError as e:
            return _invalid(e)
        except FieldAdapterError as e:
            # A field adapter refuses a value the form cannot build (for example base64 bytes for a message field).
            return {"ok": False, "kind": "invalid", "fields": {}, "form": [str(e)]}
        except VersionConflict:
            return {"ok": False, "kind": "conflict"}
        except DoesNotExist:
            return {"ok": False, "kind": "not_found"}

    return per_row(write)


def service_function(model: Any, config: Mapping[str, Any], op: str, fn: Callable[..., Any], models: Mapping[str, Any] | None = None) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function of one service function (``service.<Resource>.<op>``)."""
    sync = is_sync(fn)

    async def call(database: Any, args: dict[str, Any]) -> Any:
        ctx = _context(database, args, model, models)
        ctx._loop = asyncio.get_running_loop()
        # A bulk delete sends a query set: `values` are the binds of its filters, not field values.
        values = {} if "filters" in args else decode_values(model, args.get("values") or {})
        user = args.get("user") or {}
        protect = allow_writes(model) if args.get("allow_protected") else contextlib.nullcontext()
        try:
            with protect:
                if op == "create":
                    arguments: tuple[Any, ...] = (ctx, values)
                else:
                    if "filters" in args:
                        qs = _query_set(model, database, args)
                    else:
                        qs = _one_row(model, database, args["key"], await _resource_filters(config, user, op))
                    arguments = (ctx, qs, values) if op == "update" else (ctx, qs)
                if sync:
                    # The worker thread needs the context variables (the protected-write opt-in) of this task.
                    scope = contextvars.copy_context()
                    result = await asyncio.get_running_loop().run_in_executor(None, lambda: scope.run(fn, *arguments))
                else:
                    result = await fn(*arguments)
        except ValidationError as e:
            return _invalid(e)
        except VersionConflict:
            return {"ok": False, "kind": "conflict"}
        except DoesNotExist:
            return {"ok": False, "kind": "not_found"}
        if inspect.isawaitable(result):
            if inspect.iscoroutine(result):
                result.close()
            return {
                "ok": False,
                "kind": "sync_service",
                "message": f"the {op} service of {model.__name__} is a threaded sync function that returned an awaitable: run the async database calls with ctx.run(...)",
            }
        out: dict[str, Any] = {"ok": True, "audited": ctx._audited, "audit_keys": ctx._audit_keys}
        if op == "create":
            if result is None:
                return {"ok": False, "kind": "error", "message": f"the create service of {model.__name__} returns the new record"}
            out["key"] = _key_json(model, result)
        return out

    return per_row(call)


def writer_host(resources: Mapping[str, tuple[Any, Mapping[str, Any]]], models: Mapping[str, Any] | None = None) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function ``admin.write``: one function for every resource, chosen by the model name of the call.

    ``models`` are the child models of the inlines: one with no page of its own gets a writer with no filters."""
    writers = {name: default_writer(model, {}) for name, model in (models or {}).items()}
    writers.update({name: default_writer(model, config) for name, (model, config) in resources.items()})

    async def write(database: Any, args: dict[str, Any]) -> Any:
        return await writers[args["model"]](database, args)

    return write


# -- actions ---------------------------------------------------------------------------------------------------------

_PLACEMENTS = ("bulk", "row", "detail", "list", "submit")
_ACTION = "_monorm_admin_action"
_LEVELS = ("info", "success", "warning", "error")


def message(text: str, level: str = "info") -> dict[str, Any]:
    """The result of an action that shows a message: ``level`` is ``info``, ``success``, ``warning`` or ``error``."""
    if level not in _LEVELS:
        raise ValueError(f"the level of a message is one of {', '.join(_LEVELS)}")
    return {"kind": "message", "message": text, "level": level}


def redirect(to: str) -> dict[str, Any]:
    """The result of an action that sends the browser to a path of the admin site (it starts with ``/``)."""
    return {"kind": "redirect", "to": to}


def download(filename: str, content: bytes | str, content_type: str = "application/octet-stream") -> dict[str, Any]:
    """The result of an action that sends a file to the browser."""
    data = content.encode() if isinstance(content, str) else bytes(content)
    return {"kind": "download", "filename": filename, "content_type": content_type, "base64": base64.b64encode(data).decode("ascii")}


def refresh() -> dict[str, Any]:
    """The result of an action that only reloads the page. A function that returns ``None`` has this result."""
    return {"kind": "refresh"}


def _result_json(result: Any) -> dict[str, Any]:
    if result is None:
        return refresh()
    if isinstance(result, str):
        return message(result)
    if isinstance(result, Mapping) and "kind" in result:
        return dict(result)
    raise TypeError("an action returns None, a message string or one of admin.message, admin.redirect, admin.download and admin.refresh")


class ActionDef:
    """The definition of one action; see :func:`action`."""

    def __init__(
        self,
        fn: Callable[..., Any] | None,
        name: str,
        *,
        label: str | None,
        placement: str | None,
        permission: str | None,
        confirm: str | None,
        inputs: tuple[InputDef, ...],
        applies_to: Any,
    ) -> None:
        self.fn = fn
        self.name = name
        self.label = label
        self.placement = placement
        self.permission = permission
        self.confirm = confirm
        self.inputs = inputs
        self.applies_to = applies_to


def action(
    fn: Callable[..., Any] | None = None,
    *,
    name: str | None = None,
    label: str | None = None,
    placement: str = "bulk",
    permission: str | None = None,
    confirm: str | None = None,
    inputs: tuple[InputDef, ...] | list[InputDef] = (),
    applies_to: Any = None,
) -> Any:
    """Marks a function as an admin action. Use it as a decorator: ``@admin.action(placement="bulk", confirm="Refund these orders?")``.

    The function is ``async def refund(ctx, qs)``: ``qs`` is the query set of the rows (with the ``queryset`` filter, the ``can`` rule
    and ``applies_to``), ``ctx.inputs`` holds the values of ``inputs`` and ``ctx.count`` the number of rows. Loop with
    ``async for batch in qs.batches(500)``. The function returns ``None`` (a refresh), a string (a message) or ``admin.message``,
    ``admin.redirect``, ``admin.download`` or ``admin.refresh``. The decorator returns the function itself: a unit test calls it with a
    ``Context`` and a query set, and needs no admin.

    ``placement`` is ``bulk`` (a selection or all rows that match), ``row``, ``detail``, ``list`` (all rows that match the filters of
    the list) or ``submit`` (next to the save button of the form). ``permission`` is the name part of ``<label>.<permission>_<resource>``
    (``permission="refund"``); an action with no ``permission`` needs the manage permission of the resource. ``applies_to`` is a condition: the action is off on the rows where it is false, and the query set leaves them out.
    ``name`` is the name of the function by default.
    """

    def mark(f: Callable[..., Any]) -> Callable[..., Any]:
        n = name or f.__name__
        setattr(f, _ACTION, ActionDef(f, n, label=label, placement=placement, permission=permission, confirm=confirm, inputs=tuple(inputs), applies_to=applies_to))
        return f

    return mark(fn) if fn is not None else mark


def action_def(item: Any) -> ActionDef | None:
    """The definition that ``@admin.action`` put on a function, or the definition itself."""
    if isinstance(item, ActionDef):
        return item
    found = getattr(item, _ACTION, None)
    return found if isinstance(found, ActionDef) else None


def _untag(value: Any) -> Any:
    """A bind of the engine (``{"$": tag, "v": ...}`` for the types that JSON cannot tell apart) as a Python value."""
    if not (isinstance(value, dict) and "$" in value):
        return value
    tag, raw = value["$"], value["v"]
    if tag == "bigint":
        return int(raw)
    if tag == "datetime":
        return _decode("date_time", raw)
    if tag == "date":
        return _decode("date", raw)
    if tag == "time":
        return _decode("time", raw)
    if tag == "uuid":
        return uuid.UUID(raw)
    if tag == "decimal":
        return decimal.Decimal(raw)
    if tag == "bytes":
        return base64.b64decode(raw)
    if tag == "array":
        return [_untag(v) for v in raw]
    return raw


class _IrFilter(Condition):
    """A filter that Rust built (engine IR) with the values of its parameters."""

    __slots__ = ("ir", "values")

    def __init__(self, ir: Any, values: list[Any]) -> None:
        self.ir = ir
        self.values = values

    def _ir(self, ctx: IRContext) -> Any:
        return self._remap(self.ir, ctx.params)

    def _remap(self, ir: Any, params: list[Any]) -> Any:
        if isinstance(ir, list):
            return [self._remap(i, params) for i in ir]
        if not isinstance(ir, dict):
            return ir
        if ir.get("t") in ("param", "in_param"):
            params.append(self.values[ir["i"]])
            return {**ir, "i": len(params) - 1}
        return {k: self._remap(v, params) for k, v in ir.items()}

    def __repr__(self) -> str:
        return f"filter({self.ir!r})"


def _query_set(model: Any, db: Any, args: Mapping[str, Any]) -> Any:
    """The query set of an action call: the filters of the payload, on the database of the request."""
    values = [_untag(v) for v in args.get("values") or []]
    qs = model.objects.using(db)
    filters = [_IrFilter(f, values) for f in args.get("filters") or []]
    # An action on every row, with no filter, is a write the staff asked for: `.all()` lets `qs.update(...)` run.
    qs = qs.filter(*filters) if filters else qs.all()
    if args.get("without"):
        qs = qs.without_defaults(*args["without"])
    return qs


def action_function(model: Any, config: Mapping[str, Any], d: ActionDef, models: Mapping[str, Any] | None = None) -> Callable[[Any, dict[str, Any]], Awaitable[Any]]:
    """The host function of one action (``action.<Resource>.<name>``)."""
    fn = d.fn
    assert fn is not None
    sync = is_sync(fn)

    async def call(database: Any, args: dict[str, Any]) -> Any:
        ctx = _context(database, args, model, models)
        ctx._loop = asyncio.get_running_loop()
        ctx.op = f"action:{d.name}"  # the default action of ctx.audit
        protect = allow_writes(model) if args.get("allow_protected") else contextlib.nullcontext()
        try:
            with protect:
                qs = _query_set(model, database, args)
                if sync:
                    scope = contextvars.copy_context()
                    result = await asyncio.get_running_loop().run_in_executor(None, lambda: scope.run(fn, ctx, qs))
                else:
                    result = await fn(ctx, qs)
        except ValidationError as e:
            return _invalid(e)
        except VersionConflict:
            return {"ok": False, "kind": "conflict"}
        except DoesNotExist:
            return {"ok": False, "kind": "not_found"}
        if inspect.isawaitable(result):
            if inspect.iscoroutine(result):
                result.close()
            return {"ok": False, "kind": "sync_service", "message": f"the action {d.name} of {model.__name__} is a threaded sync function that returned an awaitable: run the async database calls with ctx.run(...)"}
        try:
            shown = _result_json(result)
        except TypeError as e:
            return {"ok": False, "kind": "error", "message": f"the action {d.name} of {model.__name__}: {e}"}
        return {"ok": True, "audited": ctx._audited, "audit_keys": ctx._audit_keys, "result": shown}

    return call
