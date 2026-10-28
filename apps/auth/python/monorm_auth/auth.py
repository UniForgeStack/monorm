"""`authenticate()`."""

from __future__ import annotations

from typing import Any

from monorm import Database, Registry

from .hashers import check_password, make_password
from .permissions import _active_model

# Checked when the user does not exist, so the time does not show whether a username exists.
_DUMMY = make_password("monorm-auth dummy password")


async def authenticate(
    username: str,
    password: str,
    *,
    using: Database | None = None,
    registry: Registry | None = None,
    model: Any | None = None,
) -> Any | None:
    """The active user with this username and password, else `None`. An old hash is replaced by an argon2id hash.

    `model` is the user model. The default is the model that overrides `User`, else `User`; an `@@inherit.extends` child needs it.
    """
    User = model if model is not None else _active_model("User", registry)
    user = await User.objects.using(using).filter(User.username == username).first()
    if user is None:
        check_password(password, _DUMMY)
        return None
    old = user.password
    if not user.check_password(password, update=True) or not user.is_active:
        return None
    if user.password != old:
        await user.update(password=user.password)
    return user
