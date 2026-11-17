from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Note(_message.Message):
    __slots__ = ("title", "priority", "tags", "inner")
    TITLE_FIELD_NUMBER: _ClassVar[int]
    PRIORITY_FIELD_NUMBER: _ClassVar[int]
    TAGS_FIELD_NUMBER: _ClassVar[int]
    INNER_FIELD_NUMBER: _ClassVar[int]
    title: str
    priority: int
    tags: _containers.RepeatedScalarFieldContainer[str]
    inner: Outer.Inner
    def __init__(self, title: _Optional[str] = ..., priority: _Optional[int] = ..., tags: _Optional[_Iterable[str]] = ..., inner: _Optional[_Union[Outer.Inner, _Mapping]] = ...) -> None: ...

class Outer(_message.Message):
    __slots__ = ()
    class Inner(_message.Message):
        __slots__ = ("label",)
        LABEL_FIELD_NUMBER: _ClassVar[int]
        label: str
        def __init__(self, label: _Optional[str] = ...) -> None: ...
    def __init__(self) -> None: ...

class Other(_message.Message):
    __slots__ = ("name",)
    NAME_FIELD_NUMBER: _ClassVar[int]
    name: str
    def __init__(self, name: _Optional[str] = ...) -> None: ...

class NoteV2(_message.Message):
    __slots__ = ("title", "priority", "tags", "added_later")
    TITLE_FIELD_NUMBER: _ClassVar[int]
    PRIORITY_FIELD_NUMBER: _ClassVar[int]
    TAGS_FIELD_NUMBER: _ClassVar[int]
    ADDED_LATER_FIELD_NUMBER: _ClassVar[int]
    title: str
    priority: int
    tags: _containers.RepeatedScalarFieldContainer[str]
    added_later: str
    def __init__(self, title: _Optional[str] = ..., priority: _Optional[int] = ..., tags: _Optional[_Iterable[str]] = ..., added_later: _Optional[str] = ...) -> None: ...
