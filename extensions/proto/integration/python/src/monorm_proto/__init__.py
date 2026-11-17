"""Protobuf message fields on a Bytes column: the codec of a message class, found from its API or registered."""

from __future__ import annotations
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any
from monorm.adapters import FieldAdapterError


class ProtoError(FieldAdapterError):
    """A protobuf field that cannot read or write a value: a wrong class, no codec, or bytes that do not decode."""


@dataclass(frozen=True)
class Codec:
    """The four functions of one message class. The JSON pair is optional."""

    encode: Callable[[Any], bytes]
    decode: Callable[[bytes], Any]
    to_json: Callable[[Any], Any] | None = None
    from_json: Callable[[Any], Any] | None = None


_REGISTRY: dict[type, Codec] = {}


def proto_codec(
    cls: type,
    encode: Callable[[Any], bytes],
    decode: Callable[[bytes], Any],
    to_json: Callable[[Any], Any] | None = None,
    from_json: Callable[[Any], Any] | None = None,
) -> None:
    """Register the codec of a message class that no known protobuf library made. It wins over detection."""
    _REGISTRY[cls] = Codec(encode, decode, to_json, from_json)


def _google(cls: type) -> Codec | None:
    # The Google protobuf API: `SerializeToString` on the instance, `FromString` on the class.
    if not (callable(getattr(cls, "FromString", None)) and callable(getattr(cls, "SerializeToString", None))):
        return None
    to_json = from_json = None
    try:
        from google.protobuf import json_format
    except ImportError:
        pass
    else:
        to_json = json_format.MessageToDict
        from_json = lambda value: json_format.ParseDict(value, cls())  # noqa: E731
    return Codec(lambda message: message.SerializeToString(), cls.FromString, to_json, from_json)  # type: ignore[attr-defined]


def codec_for(cls: type) -> Codec:
    """The registered codec of ``cls``, else the one its API shows; an error that names ``proto_codec`` when there is none."""
    found = _REGISTRY.get(cls) or _google(cls)
    if found is None:
        raise ProtoError(
            f"no codec for {cls.__module__}.{cls.__qualname__}: it has no known protobuf API; "
            "register one with monorm_proto.proto_codec(cls, encode, decode)"
        )
    return found


__all__ = ["Codec", "ProtoError", "codec_for", "proto_codec"]
