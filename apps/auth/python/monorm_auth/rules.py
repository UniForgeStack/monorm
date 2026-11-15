"""The built-in password rules. Each one runs the Rust check (`monorm_core::password`), the same code as the TypeScript twin; `Breached` makes a network call in Python instead."""

from __future__ import annotations

import asyncio
import hashlib
import http.client
import json
import logging
import urllib.request
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any, Literal

from monorm import _native

_log = logging.getLogger("monorm_auth")

__all__ = ["Breached", "Common", "MinLength", "Numeric", "PasswordError", "UserSimilarity"]


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
    """A rule that the Rust function checks and runs, except `Breached`, which only takes its options from it. The options are checked when the rule is made."""

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


class Breached(_Builtin):
    """Not in a known data breach: the k-anonymity range query of the Pwned Passwords API (opt-in, a network call).

    The server gets the first 5 hex characters of the SHA-1 hash only. Refuses when the breach count is above
    `threshold`. A network error accepts the password (`fail="open"`, with a warning in the log) or refuses it
    with `password_check_unavailable` (`fail="closed"`).
    """

    name = "breached"

    def __init__(self, threshold: int = 0, host: str = "https://api.pwnedpasswords.com", timeout: float = 2.0, fail: Literal["open", "closed"] = "open") -> None:
        super().__init__(threshold=threshold, host=host, timeout=timeout, fail=fail)
        self._threshold, self._host, self._timeout, self._fail = threshold, host.rstrip("/"), timeout, fail

    def _fetch(self, prefix: str) -> str:
        request = urllib.request.Request(f"{self._host}/range/{prefix}", headers={"Add-Padding": "true", "User-Agent": "monorm-auth"})
        with urllib.request.urlopen(request, timeout=self._timeout) as response:  # noqa: S310 - the host is project settings
            if response.status != 200:
                raise OSError(f"status {response.status}")
            body: str = response.read().decode("utf-8")
            return body

    async def validate(self, password: str, user: Any = None) -> list[PasswordError]:  # type: ignore[override]
        digest = hashlib.sha1(password.encode("utf-8"), usedforsecurity=False).hexdigest().upper()
        prefix, suffix = digest[:5], digest[5:]
        try:
            body = await asyncio.wait_for(asyncio.to_thread(self._fetch, prefix), self._timeout)
            count = 0
            for line in body.splitlines():
                found, _, n = line.strip().partition(":")
                if found.upper() == suffix:
                    n = n.strip()
                    # int() also takes "-5", "1_000" and non-ASCII digits.
                    if not (n.isascii() and n.isdigit()):
                        raise ValueError("bad count")
                    count = int(n)
                    break
        except (OSError, ValueError, http.client.HTTPException) as e:
            # Never log the URL: it holds the hash prefix.
            _log.warning("breached password check unavailable (%s): %s", self._host, type(e).__name__)
            if self._fail == "open":
                return []
            return [PasswordError("password_check_unavailable", "The password could not be checked against known data breaches. Try again later.")]
        if count > self._threshold:
            return [PasswordError("password_breached", "This password appeared in a known data breach. Choose another one.", {"count": count})]
        return []
