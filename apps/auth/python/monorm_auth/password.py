"""Password rules: `validate_password` runs the rules of `[auth] password_rules`, or the rules that the caller gives.

A rule has `validate(password, user)`, sync or async, which returns a list of `PasswordError` (it does not raise
for a weak password), and `help_text()`. The built-in rules are in `rules`; a custom rule is `module:attr` in the
settings: a class or a function that gets the other keys of its entry as keyword arguments.
"""

from __future__ import annotations

import importlib
import inspect
import json
import os
from collections.abc import Awaitable, Sequence
from typing import Any, Protocol

from monorm import _native

from . import rules as rules
from .rules import PasswordError

__all__ = ["PasswordRule", "load_rules", "password_help_texts", "validate_password"]

_BUILT_IN: dict[str, Any] = {
    "min_length": rules.MinLength,
    "common": rules.Common,
    "numeric": rules.Numeric,
    "user_similarity": rules.UserSimilarity,
    "breached": rules.Breached,
}


class PasswordRule(Protocol):
    def validate(self, password: str, user: Any = None) -> list[PasswordError] | Awaitable[list[PasswordError]]: ...

    def help_text(self) -> str: ...


def _custom(spec: str, options: dict[str, Any]) -> PasswordRule:
    module_name, _, attr = spec.partition(":")
    try:
        obj: Any = importlib.import_module(module_name)
        for part in attr.split("."):
            obj = getattr(obj, part)
        rule: Any = obj(**options) if callable(obj) else obj
    except (ImportError, AttributeError, TypeError) as e:
        raise RuntimeError(f"password rule `{spec}`: {e}") from None
    if not callable(getattr(rule, "validate", None)) or not callable(getattr(rule, "help_text", None)):
        raise RuntimeError(f"password rule `{spec}` has no validate() and help_text()")
    return rule  # type: ignore[no-any-return]


def load_rules(directory: str | None = None) -> list[PasswordRule]:
    """The rules of `[auth] password_rules` in the project settings of `directory` (default: the current directory).

    No setting gives the default rules; an empty list gives no rules. A wrong setting, or a custom rule that
    cannot be imported, raises here and not at the first password.
    """
    entries = json.loads(_native.password_rules(directory or os.getcwd()))
    out: list[PasswordRule] = []
    for entry in entries:
        name, options = entry["rule"], {k: v for k, v in entry.items() if k != "rule"}
        out.append(_custom(name, options) if ":" in name else _BUILT_IN[name](**options))
    return out


async def validate_password(password: str, user: Any = None, *, rules: Sequence[PasswordRule] | None = None) -> list[PasswordError]:
    """Runs every rule in list order and returns all errors; an empty list means the password is good.

    `user` is a user model, a dict or `None` (for example before the row exists). `create_user` and
    `set_password` do not call this: forms and commands do.
    """
    errors: list[PasswordError] = []
    for rule in load_rules() if rules is None else rules:
        found = rule.validate(password, user)
        errors.extend(await found if inspect.isawaitable(found) else found)
    return errors


def password_help_texts(rules: Sequence[PasswordRule] | None = None) -> list[str]:
    """The help text of each rule, in list order, for a form."""
    return [rule.help_text() for rule in (load_rules() if rules is None else rules)]
