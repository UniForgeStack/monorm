"""The host functions of the admin service: login, the user read at a session renewal, and password change through the auth app.

Each function takes ``(database, args)`` and runs in ``Database.bind_request``, so it sees the
transaction of the request. The Rust service never hashes or compares a password.
"""

from __future__ import annotations

import functools
import os
import secrets
from typing import TYPE_CHECKING, Any

from monorm_auth import authenticate, validate_password
from monorm_auth.password import PasswordRule, load_rules
from monorm_auth.permissions import _active_model

if TYPE_CHECKING:
    from .model import Registry


@functools.lru_cache(maxsize=1)
def _rules(directory: str) -> list[PasswordRule]:
    return load_rules(directory)


def load_password_rules() -> list[PasswordRule]:
    """Loads `[auth] password_rules` once per process, so a wrong setting fails at start and a custom rule keeps its state."""
    return _rules(os.getcwd())


def _user_json(user: Any) -> dict[str, Any]:
    # ``password_hash`` never reaches the cookie: the service keeps a keyed tag of it, so a changed password ends the session.
    return {
        "password_hash": user.password,
        "key": user.id,
        "username": user.username,
        "display": user.username,
        "is_superuser": bool(user.is_superuser),
        "is_active": bool(user.is_active),
    }


def auth_user_table(registry: Registry) -> str | None:
    try:
        user = _active_model("User", registry)
    except LookupError:
        return None
    meta = user._meta
    table = meta.table
    return table if isinstance(table, str) and {"roles", "permissions"} <= meta.relations.keys() else None


async def read_user(database: Any, args: dict[str, Any]) -> dict[str, Any] | None:
    user_model = _active_model("User", database._registry)
    user = await user_model.objects.using(database).filter(user_model.id == args["key"]).first()
    return None if user is None else _user_json(user)


async def authenticate_user(database: Any, args: dict[str, Any]) -> dict[str, Any] | None:
    user = await authenticate(args["username"], args["password"], using=database, registry=database._registry)
    return None if user is None else _user_json(user)


async def change_password(database: Any, args: dict[str, Any]) -> dict[str, Any]:
    user_model = _active_model("User", database._registry)
    user = await user_model.objects.using(database).filter(user_model.id == args["key"]).first()
    if user is None or not user.is_active:
        return {"changed": False, "reason": "the user cannot change the password"}
    if not user.check_password(args["old_password"]):
        return {"changed": False, "reason": "the old password is wrong"}
    return await _store_password(user, args["new_password"])


async def set_password(database: Any, args: dict[str, Any]) -> dict[str, Any]:
    user_model = _active_model("User", database._registry)
    user = await user_model.objects.using(database).filter(user_model.id == args["key"]).first()
    if user is None:
        return {"changed": False, "reason": "no such user"}
    return await _store_password(user, args["new_password"])


async def _store_password(user: Any, password: str) -> dict[str, Any]:
    errors = await validate_password(password, user, rules=load_password_rules())
    if errors:
        # The codes and params reach the browser and the audit row, so a rule must keep the password out of them.
        return {
            "changed": False,
            "reason": " ".join(e.message for e in errors),
            "errors": [{"code": e.code, "message": e.message, "params": e.params} for e in errors],
        }
    user.set_password(password)
    await user.update(password=user.password)
    return {"changed": True, "password_hash": user.password}


async def sso_login(database: Any, args: dict[str, Any]) -> dict[str, Any]:
    """The user of a verified single sign-on login: found by username, else created, and the role membership set to the roles of the claim.

    An inactive user is returned as it is and nothing changes. A name in ``roles`` that no role has grants nothing.
    A new user gets a random unusable password marker, so only the single sign-on reaches the account.
    """
    registry = database._registry
    user_model = _active_model("User", registry)
    role_model = _active_model("Role", registry)
    join = _active_model(f"{user_model.__name__}Role", registry)
    users = user_model.objects.using(database)
    user = await users.filter(user_model.username == args["username"]).first()
    created = user is None
    if user is None:
        row = {"username": args["username"], "email": args.get("email") or "", "password": "!" + secrets.token_urlsafe(32)}
        await users.insert_many([row]).on_conflict(user_model.username, update=False)
        user = await users.filter(user_model.username == args["username"]).first()
        assert user is not None
    elif not user.is_active:
        return {"user": _user_json(user), "created": False}
    names = list(dict.fromkeys(args.get("roles") or []))
    wanted = {r.id for r in await role_model.objects.using(database).filter(role_model.name.in_(names))} if names else set()
    held = join.objects.using(database).filter(join.user_id == user.id)
    current = {j.role_id for j in await held}
    if current - wanted:
        await held.filter(join.role_id.in_(sorted(current - wanted))).delete()
    if wanted - current:
        await join.objects.using(database).insert_many([{"user_id": user.id, "role_id": r} for r in sorted(wanted - current)])
    return {"user": _user_json(user), "created": created}


HOST = {
    "auth.authenticate": authenticate_user,
    "auth.user": read_user,
    "auth.change_password": change_password,
    "auth.set_password": set_password,
    "auth.sso_login": sso_login,
}
