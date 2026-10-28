"""The `Auth` test harness: models loaded from a consumer schema that imports the app."""

from dataclasses import dataclass
from typing import Any

import monorm


@dataclass(frozen=True)
class Variant:
    """A consumer schema: the `schema` text that adds a user model, its `model` name, and the `base` name of the join models."""

    schema: str
    model: str
    base: str
    extends: bool = False


# an override keeps the join models of `User`; an `extends` child gets its own (`AccountRole`, `account_id`)
VARIANTS = {
    "plain": Variant("", "User", "User"),
    "override": Variant('model MyUser {\n  bio String?\n  @@inherit.override("User")\n}\n', "MyUser", "User"),
    "extends": Variant(
        'model Account {\n  bio String?\n  @@inherit.extends("AbstractUser")\n}\n', "Account", "Account", extends=True
    ),
}


class Auth:
    def __init__(self, db: monorm.Database, registry: monorm.Registry, models: dict[str, Any], variant: Variant) -> None:
        self.db, self.registry, self.models, self.variant = db, registry, models, variant
        self.User = models[variant.model]
        self.Role, self.Permission = models["Role"], models["Permission"]
        self.UserRole, self.UserPermission = models[f"{variant.base}Role"], models[f"{variant.base}Permission"]
        self.user_id = f"{variant.base.lower()}_id"

    @property
    def kw(self) -> dict[str, Any]:
        """The arguments of `authenticate`: the registry cannot find an `extends` child, so the model is passed."""
        kw: dict[str, Any] = {"using": self.db, "registry": self.registry}
        if self.variant.extends:
            kw["model"] = self.User
        return kw

    async def seed(self) -> dict[str, Any]:
        """Users `staff` (a role with add_post), `direct` (change_post), `root` (superuser), `idle` (inactive, has the role)."""
        m, db = self.models, self.db
        permissions = (
            await self.Permission.objects.using(db)
            .insert_many(
                [
                    {"label": "blog", "codename": "add_post", "name": "Can add post"},
                    {"label": "blog", "codename": "change_post", "name": "Can change post"},
                    {"label": "shop", "codename": "add_post", "name": "Can add post"},
                ]
            )
            .returning()
        )
        (role,) = await self.Role.objects.using(db).insert_many([{"name": "editor"}]).returning()
        await (
            m["RolePermission"]
            .objects.using(db)
            .insert_many([{"role_id": role.id, "permission_id": permissions[0].id}])
        )
        staff, direct, root, idle = (
            await self.User.objects.using(db)
            .insert_many(
                [
                    {"username": "staff", "email": "s@x.y", "password": "!"},
                    {"username": "direct", "email": "d@x.y", "password": "!"},
                    {"username": "root", "email": "r@x.y", "password": "!", "is_superuser": True},
                    {"username": "idle", "email": "i@x.y", "password": "!", "is_active": False, "is_superuser": True},
                ]
            )
            .returning()
        )
        uid = self.user_id
        await self.UserRole.objects.using(db).insert_many(
            [{uid: staff.id, "role_id": role.id}, {uid: idle.id, "role_id": role.id}]
        )
        await self.UserPermission.objects.using(db).insert_many([{uid: direct.id, "permission_id": permissions[1].id}])
        return {"staff": staff, "direct": direct, "root": root, "idle": idle, "role": role}
