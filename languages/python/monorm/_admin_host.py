"""The host functions of the admin service: login, the user read at a session renewal, and password change through the auth app.

Each function takes ``(database, args)`` and runs in ``Database.bind_request``, so it sees the
transaction of the request. The Rust service never hashes or compares a password.
"""

from __future__ import annotations

import functools
import os
from typing import Any

from monorm_auth import authenticate, validate_password
from monorm_auth.password import PasswordRule, load_rules
from monorm_auth.permissions import _active_model


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
    errors = await validate_password(args["new_password"], user, rules=load_password_rules())
    if errors:
        # The codes and params reach the browser and the audit row, so a rule must keep the password out of them.
        return {
            "changed": False,
            "reason": " ".join(e.message for e in errors),
            "errors": [{"code": e.code, "message": e.message, "params": e.params} for e in errors],
        }
    user.set_password(args["new_password"])
    await user.update(password=user.password)
    return {"changed": True, "password_hash": user.password}


HOST = {"auth.authenticate": authenticate_user, "auth.user": read_user, "auth.change_password": change_password}
