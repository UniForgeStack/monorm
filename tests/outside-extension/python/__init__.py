"""An outside package's runtime; its pass library is used only at compile time."""
from collections.abc import Mapping
from typing import Any, Self

from monorm import QuerySet
from monorm.adapters import Pending, register_field_adapter
from monorm.ext import ModelT

MANIFEST_HASH = "feb1941ebdbd1c55d4a3233c741b85b89b714f1f8eb907fc24a43e0bab78afce"


class OutsideQuerySet(QuerySet[ModelT]):
    def tagged(self) -> Self:
        return self.limit(7)


class OutsideInstance:
    def tag_label(self) -> str:
        return str(getattr(self, "tag"))


class Wrapped:
    """What an `acme.outside.wrapped.v1` field shows: the stored value with a label."""

    def __init__(self, label: str, payload: object):
        self.label = label
        self.payload = payload


class _Runtime:
    def __init__(self) -> None:
        self.prefix = ""

    def configure(self, prefix: str) -> None:
        self.prefix = prefix


class _WrappedAdapter:
    def __init__(self, runtime: _Runtime, prefix: str):
        self.runtime, self.prefix = runtime, prefix

    def validate(self, value: Any) -> Any | Pending:
        return value.payload if isinstance(value, Wrapped) else value

    async def prepare(self, value: Any) -> Any:
        return value

    def decode(self, stored: Any) -> Wrapped | None:
        return None if stored is None else Wrapped(self.runtime.prefix + self.prefix, stored)


def install(model: type[Any], fields: Mapping[str, tuple[Mapping[str, Any], bool]]) -> _Runtime:
    """The generated-code entry for the `acme.outside.wrapped.v1` adapter."""
    runtime = _Runtime()
    for name, (options, _nullable) in fields.items():

        def unwrap(self: Any, _name: str = name) -> object:
            return getattr(self, _name).payload

        register_field_adapter(model, name, _WrappedAdapter(runtime, options["prefix"]), {f"{name}_unwrap": unwrap})
    return runtime
