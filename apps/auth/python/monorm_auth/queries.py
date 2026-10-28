"""`UserQueries`: the query set of every user model."""

from __future__ import annotations

from typing import Any, Self

from monorm import Condition, QuerySet

from .permissions import split_perm


def perm_condition(user: Any, perm: str) -> Condition:
    """The condition "an active user of the model `user` that has `perm`": directly, through a role, or as a superuser."""
    label, codename = split_perm(perm)
    direct, via_role = user.permissions, user.roles.permissions
    has = ((direct.label == label) & (direct.codename == codename)) | (
        (via_role.label == label) & (via_role.codename == codename)
    )
    condition: Condition = (user.is_active == True) & ((user.is_superuser == True) | has)  # noqa: E712
    return condition


class UserQueries(QuerySet[Any]):
    def with_perm(self, perm: str) -> Self:
        """Active users that have `perm` (`"blog.add_post"`): directly, through a role, or as a superuser."""
        return self.filter(perm_condition(self.model, perm))
