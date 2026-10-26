"""An outside package's runtime; its pass library is used only at compile time."""
from typing import Self

from orm import QuerySet
from orm.ext import ModelT

MANIFEST_HASH = "7d1acfa50bb44f0ffc2faa307c15400b1b65ce637dc254b80137421605bc7306"


class OutsideQuerySet(QuerySet[ModelT]):
    def tagged(self) -> Self:
        return self.limit(7)


class OutsideInstance:
    def tag_label(self) -> str:
        return str(getattr(self, "tag"))
