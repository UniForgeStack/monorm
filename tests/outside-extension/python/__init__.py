"""An outside package's runtime; its pass library is used only at compile time."""
from typing import Self

from orm import QuerySet
from orm.ext import ModelT

MANIFEST_HASH = "eae14354d3d5506de40f184c1c834c24f8cb3171a24729cfc1cc1e6ff899f61d"


class OutsideQuerySet(QuerySet[ModelT]):
    def tagged(self) -> Self:
        return self.limit(7)


class OutsideInstance:
    def tag_label(self) -> str:
        return str(getattr(self, "tag"))
