from __future__ import annotations

from typing import TypeVar

from typing_extensions import Self

from orm import QuerySet
from orm.ext import ModelLike, ModelT

R = TypeVar("R", bound=ModelLike)


class TaggedQuerySet(QuerySet[ModelT]):
    def everything(self) -> Self:
        return self.without_defaults()


class TaggedInstance:
    def label(self: ModelLike) -> str:
        return str(self.pk)

    def me(self: R) -> R:
        return self
