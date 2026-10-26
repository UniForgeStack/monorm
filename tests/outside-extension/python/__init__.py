"""An outside package's runtime; its pass library is used only at compile time."""
from typing import Self

from orm import QuerySet
from orm.ext import ModelT

MANIFEST_HASH = "4795be01f8107922ca014e8766850287849e0d7f7e323cb16cc24c141067a866"


class OutsideQuerySet(QuerySet[ModelT]):
    def tagged(self) -> Self:
        return self.limit(7)


class OutsideInstance:
    def tag_label(self) -> str:
        return str(getattr(self, "tag"))
