"""An outside package's runtime; its pass library is used only at compile time."""
from typing import Self

from orm import QuerySet
from orm.ext import ModelT

MANIFEST_HASH = "49078d804127b38d85523fa45f79c7259143261ba617603a6d273b2864ce5295"


class OutsideQuerySet(QuerySet[ModelT]):
    def tagged(self) -> Self:
        return self.limit(7)


class OutsideInstance:
    def tag_label(self) -> str:
        return str(getattr(self, "tag"))
