"""The unique indexes of a model, for ``on_conflict(index=...)``: ``Task.indexes.task_one_open``."""

from __future__ import annotations

from typing import Any

__all__ = ["IndexRef"]


class IndexRef:
    """A unique index of one model, named by its ``map:`` name or its generated name."""

    __slots__ = ("_model", "name")

    def __init__(self, model: type[Any], name: str) -> None:
        self._model = model
        self.name = name

    def __repr__(self) -> str:
        return f"{self._model.__name__}.indexes.{self.name}"


class _Indexes:
    """``Model.indexes``: one :class:`IndexRef` attribute per unique index."""

    __slots__ = ("_model", "_names")

    def __init__(self, model: type[Any], names: tuple[str, ...]) -> None:
        self._model = model
        self._names = names

    def __getattr__(self, name: str) -> IndexRef:
        if name in self._names:
            return IndexRef(self._model, name)
        known = ", ".join(self._names) or "none"
        raise AttributeError(f"{self._model.__name__} has no unique index {name!r}; its unique indexes: {known}")

    def __getitem__(self, name: str) -> IndexRef:
        return self.__getattr__(name)

    def __dir__(self) -> list[str]:
        return list(self._names)

    def __iter__(self) -> Any:
        return (IndexRef(self._model, n) for n in self._names)

    def __repr__(self) -> str:
        return f"<{self._model.__name__}.indexes: {', '.join(self._names)}>"
