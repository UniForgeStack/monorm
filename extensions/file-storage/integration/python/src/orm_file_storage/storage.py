"""OpenDAL-backed storage: the reference codec, the operator map and the streaming upload."""

from __future__ import annotations

import asyncio
import json
import uuid
from collections.abc import AsyncIterable, AsyncIterator, Mapping
from dataclasses import asdict, dataclass
from typing import Any, BinaryIO

from opendal import AsyncOperator
from opendal.exceptions import Unsupported

CHUNK_SIZE = 64 * 1024
MAX_SIZE = 2**53 - 1  # exact in both Python and JavaScript
DEFAULT_CONTENT_TYPE = "application/octet-stream"

Source = bytes | bytearray | memoryview | BinaryIO | AsyncIterable[bytes]


class StorageError(Exception):
    """Storage configuration or operation failure."""


class CapabilityError(StorageError):
    pass


class SizeLimitError(StorageError):
    pass


@dataclass(frozen=True)
class Reference:
    storage: str
    key: str
    version: str | None = None
    filename: str | None = None
    content_type: str | None = None
    size: int | None = None

    def __post_init__(self) -> None:
        for name in ("storage", "key"):
            value = getattr(self, name)
            if not isinstance(value, str) or not value or "\x00" in value:
                raise ValueError(f"{name} must be a nonempty string without NUL")
        for name in ("version", "filename", "content_type"):
            value = getattr(self, name)
            if value is not None and (not isinstance(value, str) or "\x00" in value):
                raise ValueError(f"{name} must be a string without NUL")
        for value in (self.storage, self.key, self.version, self.filename, self.content_type):
            if value is not None:
                value.encode("utf-8")
        if self.size is not None and (type(self.size) is not int or not 0 <= self.size <= MAX_SIZE):
            raise ValueError("size must be a nonnegative safe integer")

    def to_dict(self) -> dict[str, Any]:
        return {"v": 1, **{k: v for k, v in asdict(self).items() if v is not None}}

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), ensure_ascii=False, separators=(",", ":"))

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> Reference:
        allowed = {"v", "storage", "key", "version", "filename", "content_type", "size"}
        if type(value.get("v")) is not int or value["v"] != 1 or set(value) - allowed:
            raise ValueError("invalid storage reference version or properties")
        if any(value.get(k) is None for k in value if k != "v"):
            raise ValueError("optional reference properties must be omitted, not null")
        try:
            return cls(**{k: v for k, v in value.items() if k != "v"})
        except TypeError as error:
            raise ValueError("invalid storage reference") from error

    @classmethod
    def from_json(cls, value: str) -> Reference:
        decoded = json.loads(value)
        if not isinstance(decoded, dict):
            raise ValueError("reference must be an object")
        return cls.from_dict(decoded)


class UploadError(StorageError):
    """An upload failed. The object may exist at ``reference`` (the candidate), with no size."""

    def __init__(self, reference: Reference):
        super().__init__(f"upload failed; candidate key={reference.key}")
        self.reference = reference


async def chunks(source: Source, max_size: int = MAX_SIZE) -> AsyncIterator[bytes]:
    """Read the current stream position; never close or rewind the caller's stream."""
    if type(max_size) is not int or not 0 <= max_size <= MAX_SIZE:
        raise ValueError("max_size must be a nonnegative safe integer")
    size = 0

    async def incoming() -> AsyncIterator[bytes]:
        if isinstance(source, (bytes, bytearray, memoryview)):
            for i in range(0, len(source), CHUNK_SIZE):
                yield bytes(source[i : i + CHUNK_SIZE])
        elif isinstance(source, AsyncIterable):
            async for chunk in source:
                yield chunk
        else:
            while chunk := await asyncio.to_thread(source.read, CHUNK_SIZE):
                yield chunk

    async for chunk in incoming():
        if not isinstance(chunk, (bytes, bytearray, memoryview)):
            raise TypeError("upload streams must yield bytes")
        size += len(chunk)
        if size > max_size:
            raise SizeLimitError(f"upload exceeds {max_size} bytes")
        for i in range(0, len(chunk), CHUNK_SIZE):
            yield bytes(chunk[i : i + CHUNK_SIZE])


def check_expiry(expires_in: int) -> None:
    if type(expires_in) is not int or not 1 <= expires_in <= 604800:
        raise ValueError("expires_in must be an integer from 1 to 604800 seconds")


class Registry:
    """The application's map from storage name to OpenDAL operator. The ORM creates no operators."""

    def __init__(self, operators: Mapping[str, AsyncOperator]):
        for name in operators:
            Reference(name, "validation")
        self._operators = dict(operators)

    def operator(self, storage: str) -> AsyncOperator:
        try:
            return self._operators[storage]
        except KeyError as error:
            raise StorageError(f"unconfigured storage: {storage}") from error

    async def upload(
        self,
        storage: str,
        source: Source,
        *,
        filename: str | None = None,
        content_type: str | None = None,
        max_size: int = MAX_SIZE,
    ) -> Reference:
        operator = self.operator(storage)
        candidate = Reference(
            storage, uuid.uuid4().hex, filename=filename, content_type=content_type or DEFAULT_CONTENT_TYPE
        )
        size = 0
        try:
            # No `async with`: an error must not close the writer, which would commit the partial object.
            writer = await operator.open(candidate.key, "wb", content_type=candidate.content_type)
            async for chunk in chunks(source, max_size):
                await writer.write(chunk)
                size += len(chunk)
            await writer.close()
            version = None
            if operator.capability().read_with_version:
                version = (await operator.stat(candidate.key)).version
        except Exception as error:
            raise UploadError(candidate) from error
        return Reference(
            storage,
            candidate.key,
            version=version,
            filename=candidate.filename,
            content_type=candidate.content_type,
            size=size,
        )

    async def open(self, reference: Reference) -> AsyncIterator[bytes]:
        options = {} if reference.version is None else {"version": reference.version}
        async with await self.operator(reference.storage).open(reference.key, "rb", **options) as reader:
            while chunk := await reader.read(CHUNK_SIZE):
                yield chunk

    async def signed_url(self, reference: Reference, *, expires_in: int = 300) -> str:
        check_expiry(expires_in)
        operator = self.operator(reference.storage)
        options = {} if reference.version is None else {"version": reference.version}
        try:
            return (await operator.presign_read(reference.key, expires_in, **options)).url
        except Unsupported as error:
            raise CapabilityError(f"storage {reference.storage} does not support signed URLs") from error
