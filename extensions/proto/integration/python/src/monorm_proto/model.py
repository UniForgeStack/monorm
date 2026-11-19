"""The adapter of a protobuf field: a message is encoded to the bytes of the Bytes column and decoded when it loads."""

from __future__ import annotations
from collections.abc import Mapping
from functools import cached_property
from types import ModuleType
from typing import Any
from monorm.adapters import register_field_adapter
from . import Codec, ProtoError, codec_for


class ProtoAdapter:
    """The field adapter of one proto field. The class and its codec resolve at first use, so a model module can load before its messages."""

    def __init__(self, model: str, field: str, message: str, module: ModuleType, symbol: str):
        self.model, self.field, self.message = model, field, message
        self._module, self._symbol = module, symbol

    @cached_property
    def cls(self) -> type:
        found: Any = self._module
        for part in self._symbol.split("."):
            try:
                found = getattr(found, part)
            except AttributeError:
                raise ProtoError(f"{self.model}.{self.field}: {self._module.__name__} has no {self._symbol} for {self.message}") from None
        if not isinstance(found, type):
            raise ProtoError(f"{self.model}.{self.field}: {self._module.__name__}.{self._symbol} is not a class")
        return found

    @cached_property
    def codec(self) -> Codec:
        try:
            return codec_for(self.cls)
        except ProtoError as error:
            raise ProtoError(f"{self.model}.{self.field}: {error}") from None

    def validate(self, value: Any) -> bytes | None:
        if value is None:
            return None
        if not isinstance(value, self.cls):
            raise ProtoError(f"{self.model}.{self.field} takes {self.message}, not {type(value).__name__}")
        return self.codec.encode(value)

    async def prepare(self, value: Any) -> Any:
        raise ProtoError(f"{self.model}.{self.field}: a proto value never needs prepare")

    def decode(self, stored: Any) -> Any:
        if stored is None:
            return None
        try:
            return self.codec.decode(bytes(stored))
        except ProtoError:
            raise
        except Exception as error:
            raise ProtoError(f"{self.model}.{self.field}: the stored bytes are not a {self.message}: {error}") from error

    def admin_mode(self) -> str:
        try:
            codec = self.codec
        except ProtoError:
            return "text"
        return "json" if codec.to_json is not None and codec.from_json is not None else "text"

    def admin_to_view(self, stored: bytes) -> Any:
        codec = self.codec
        if codec.to_json is None:
            raise ProtoError(f"{self.model}.{self.field}: the codec has no JSON functions")
        message = self.decode(stored)
        try:
            return codec.to_json(message)
        except Exception as error:
            raise ProtoError(f"{self.model}.{self.field}: not a JSON message: {error}") from error

    def admin_from_view(self, value: Any) -> Any:
        codec = self.codec
        if codec.from_json is None:
            raise ProtoError(f"{self.model}.{self.field}: the codec has no JSON functions")
        try:
            return codec.from_json(value)
        except Exception as error:
            raise ProtoError(f"{self.model}.{self.field}: not a {self.message}: {error}") from error


def install(model: type[Any], fields: Mapping[str, tuple[Mapping[str, Any], bool]]) -> None:
    """The generated-code entry for the `proto.message.v1` adapter: `fields` maps a name to its options and nullability."""
    for name, (options, _nullable) in fields.items():
        adapter = ProtoAdapter(model.__name__, name, options["message"], options["module"], options["symbol"])
        register_field_adapter(model, name, adapter)
