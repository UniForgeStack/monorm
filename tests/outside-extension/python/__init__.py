"""An outside package's runtime; its pass library is used only at compile time."""
from typing import Self

from orm import QuerySet
from orm.ext import ModelT

MANIFEST_HASH = "9076582855268d3ab76646c7794e1dfd6ed4c2588d010c8c04b4be477a99ca8b"


class OutsideQuerySet(QuerySet[ModelT]):
    def tagged(self) -> Self:
        return self.limit(7)


class OutsideInstance:
    def tag_label(self) -> str:
        return str(getattr(self, "tag"))
