from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from typing import ClassVar as _ClassVar, Optional as _Optional

DESCRIPTOR: _descriptor.FileDescriptor

class Invoice(_message.Message):
    __slots__ = ("number", "cents")
    NUMBER_FIELD_NUMBER: _ClassVar[int]
    CENTS_FIELD_NUMBER: _ClassVar[int]
    number: str
    cents: int
    def __init__(self, number: _Optional[str] = ..., cents: _Optional[int] = ...) -> None: ...
