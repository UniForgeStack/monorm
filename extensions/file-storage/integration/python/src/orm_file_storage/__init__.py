"""Selected file-field binding helpers. No native/provider callback registry."""

from __future__ import annotations
from collections.abc import AsyncIterator, Callable, Mapping
from dataclasses import dataclass
from typing import Any
from orm.adapters import Pending
from orm_storage import Reference, Registry, Source, MAX_SIZE


class FileFieldError(ValueError):
    pass


class FileNotLoaded(FileFieldError):
    pass


class MissingFile(FileFieldError):
    pass


@dataclass(frozen=True)
class Upload:
    """Deferred write input: construction performs no I/O."""

    source: Source
    filename: str | None = None
    content_type: str | None = None
    max_size: int = MAX_SIZE

    def __post_init__(self) -> None:
        from collections.abc import AsyncIterable

        if not isinstance(self.source, (bytes, bytearray, memoryview, AsyncIterable)) and not callable(
            getattr(self.source, "read", None)
        ):
            raise TypeError("Upload requires bytes or a byte stream")
        Reference("validation", "validation", filename=self.filename, content_type=self.content_type)
        if type(self.max_size) is not int or not 0 <= self.max_size <= MAX_SIZE:
            raise FileFieldError("max_size must be a nonnegative safe integer")


@dataclass(frozen=True)
class FileField:
    name: str
    storage: str
    nullable: bool = False

    def decode(self, value: Any) -> Reference | None:
        if value is None:
            if not self.nullable:
                raise MissingFile(f"{self.name} is not nullable")
            return None
        reference = value if isinstance(value, Reference) else Reference.from_dict(value)
        if reference.storage != self.storage:
            raise FileFieldError(f"{self.name} requires storage {self.storage}")
        return reference

    def reference(self, snapshot: Mapping[str, Any]) -> Reference:
        if self.name not in snapshot:
            raise FileNotLoaded(f"{self.name} was not selected")
        reference = self.decode(snapshot[self.name])
        if reference is None:
            raise MissingFile(f"{self.name} has no file")
        return reference

    async def signed_url(self, snapshot: Mapping[str, Any], registry: Registry, *, expires_in: int = 300) -> str:
        reference = self.reference(snapshot)
        return await registry.resolve(reference).signed_url(reference, expires_in=expires_in)

    def open(self, snapshot: Mapping[str, Any], registry: Registry) -> AsyncIterator[bytes]:
        reference = self.reference(snapshot)
        return registry.resolve(reference).open(reference)


class FileAdapter:
    """The async field adapter of one file field: ``validate`` and ``prepare`` write, ``decode`` reads."""

    def __init__(self, field: FileField, registry: Callable[[], Registry | None]):
        self.field = field
        self._registry = registry

    def validate(self, value: Any) -> Any:
        if not isinstance(value, Upload):
            reference = self.field.decode(value)
            return None if reference is None else reference.to_dict()
        Reference(self.field.storage, "validation", filename=value.filename, content_type=value.content_type)
        self._resolve().resolve(Reference(self.field.storage, "validation"))
        return Pending(value, Reference(self.field.storage, "preflight").to_dict())

    async def prepare(self, value: Upload) -> Any:
        provider = self._resolve().resolve(Reference(self.field.storage, "validation"))
        reference = await provider.upload(
            value.source, filename=value.filename, content_type=value.content_type, max_size=value.max_size
        )
        return reference.to_dict()

    def decode(self, stored: Any) -> Reference | None:
        return self.field.decode(stored)

    def _resolve(self) -> Registry:
        registry = self._registry()
        if registry is None:
            raise FileFieldError("configure file storage before preparing uploads")
        return registry
