"""The built-in password rules. Each one runs the Rust check (`monorm_core::password`), the same code as the TypeScript twin."""

from __future__ import annotations

import json
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any

from monorm import _native

__all__ = ["Common", "MinLength", "Numeric", "PasswordError", "UserSimilarity"]


@dataclass(frozen=True)
class PasswordError:
    """One broken rule. `code` is stable: a project translates `message` by `code` and `params`.

    `params` reach the browser and the audit row: never put the password in them.
    """

    code: str
    message: str
    params: dict[str, Any] = field(default_factory=dict)


def user_attribute(user: Any, name: str) -> Any:
    """The attribute `name` of a user model, a dict or any object; `None` when there is none."""
    if user is None:
        return None
    if isinstance(user, Mapping):
        return user.get(name)
    return getattr(user, name, None)


class _Builtin:
    """A rule that the Rust function runs. The options are checked when the rule is made."""

    name: str

    def __init__(self, **options: Any) -> None:
        self._entry: dict[str, Any] = {"rule": self.name, **options}
        self.help_text()

    def _attributes(self) -> Iterable[str]:
        return ()

    def validate(self, password: str, user: Any = None) -> list[PasswordError]:
        attrs = {n: v for n in self._attributes() if isinstance(v := user_attribute(user, n), str)}
        found = json.loads(_native.password_check(json.dumps([self._entry]), password, json.dumps(attrs)))
        return [PasswordError(e["code"], e["message"], e["params"]) for e in found]

    def help_text(self) -> str:
        text: str = json.loads(_native.password_help(json.dumps([self._entry])))[0]
        return text


class MinLength(_Builtin):
    """At least `min` characters (default 8), counted as code points after NFC."""

    name = "min_length"

    def __init__(self, min: int = 8) -> None:
        super().__init__(min=min)


class Common(_Builtin):
    """Not on the list of common passwords: the bundled list, or the file `path` (one password for each line)."""

    name = "common"

    def __init__(self, path: str | None = None) -> None:
        super().__init__(**({} if path is None else {"path": path}))


class Numeric(_Builtin):
    """Not only digits."""

    name = "numeric"


class UserSimilarity(_Builtin):
    """Not too close to a user attribute: the character overlap, at least `max_similarity` (Django's quick ratio)."""

    name = "user_similarity"

    def __init__(self, attributes: Iterable[str] = ("username", "email"), max_similarity: float = 0.7) -> None:
        self._names = list(attributes)
        super().__init__(attributes=self._names, max_similarity=max_similarity)

    def _attributes(self) -> Iterable[str]:
        return self._names
