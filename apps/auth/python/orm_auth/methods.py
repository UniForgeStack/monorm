"""`UserMethods`: the methods of every user model."""

from __future__ import annotations

from collections.abc import Callable, Iterable
from typing import Any

from orm import exists, outer
from orm.ext import database_of

from .hashers import check_password, make_password, needs_update
from .permissions import _active_model
from .queries import perm_condition


class UserMethods:
    __dict__: dict[str, Any]
    id: Any
    password: str
    is_active: bool
    is_superuser: bool
    qs: Callable[[], Any]

    def set_password(self, raw: str) -> None:
        """Hash `raw` and set the field on this snapshot. It does not write the row: `await user.update(password=user.password)`."""
        self.__dict__["password"] = make_password(raw)

    def check_password(self, raw: str, update: bool = False) -> bool:
        """Compare `raw` in constant time. With `update=True`, a correct `raw` and an old hash set a new hash (not written)."""
        ok = check_password(raw, self.password)
        if ok and update and needs_update(self.password):
            self.set_password(raw)
        return ok

    async def has_perm(self, perm: str) -> bool:
        """One query. An active superuser has every permission; an inactive user has none."""
        found: bool = await self.qs().filter(perm_condition(type(self), perm)).exists()
        return found

    async def has_perms(self, perms: Iterable[str]) -> bool:
        """One query for each name, in order; it stops at the first name that the user does not have."""
        for perm in perms:
            if not await self.has_perm(perm):
                return False
        return True

    async def get_all_permissions(self) -> set[str]:
        """`"<label>.<codename>"` of the user's own permissions and those of its roles. A superuser has all."""
        if not self.is_active:
            return set()
        user: Any = type(self)
        Permission = _active_model("Permission", user._meta.registry)
        query = Permission.objects.using(database_of(self))
        if not self.is_superuser:
            # the relations of `user` (`permissions`, `roles`) have the same names in every user model, unlike the join models
            held = (user.permissions.id == outer(Permission.id)) | (user.roles.permissions.id == outer(Permission.id))
            query = query.filter(exists(user.objects.filter(user.id == self.id, held)))
        return {f"{p.label}.{p.codename}" for p in await query}
