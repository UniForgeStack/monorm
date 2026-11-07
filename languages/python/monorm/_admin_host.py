"""The host functions of the admin service: login, the user read at a session renewal, and password change through the auth app.

Each function takes ``(database, args)`` and runs in ``Database.bind_request``, so it sees the
transaction of the request. The Rust service never hashes or compares a password.
"""

from __future__ import annotations

from typing import Any

from monorm_auth import authenticate
from monorm_auth.permissions import _active_model


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
    user.set_password(args["new_password"])
    await user.update(password=user.password)
    return {"changed": True, "password_hash": user.password}


HOST = {"auth.authenticate": authenticate_user, "auth.user": read_user, "auth.change_password": change_password}
