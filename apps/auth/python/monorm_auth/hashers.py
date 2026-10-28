"""Password hashing: argon2id (PHC strings) and Django `pbkdf2_sha256$` hashes."""

from __future__ import annotations

import base64
import hashlib
import hmac
import re

from argon2 import PasswordHasher, Type
from argon2.exceptions import InvalidHashError, VerificationError

__all__ = ["check_password", "make_password", "needs_update"]

# The same parameters as `hashers.ts`, so a hash from one language does not need an update in the other.
MEMORY_KIB, TIME_COST, PARALLELISM = 65536, 3, 4

_hasher = PasswordHasher(time_cost=TIME_COST, memory_cost=MEMORY_KIB, parallelism=PARALLELISM, type=Type.ID)
_DIGITS = re.compile(r"[0-9]+")
_PARAMS = re.compile(r"^\$argon2id\$v=19\$m=(\d+),t=(\d+),p=(\d+)\$")


def make_password(raw: str) -> str:
    """Hash `raw` with argon2id and a new salt; the result is a PHC string."""
    return _hasher.hash(raw)


def check_password(raw: str, encoded: str) -> bool:
    """Compare `raw` with a stored hash in constant time. An unknown or broken hash gives `False`."""
    if encoded.startswith("$argon2"):
        try:
            return _hasher.verify(encoded, raw)
        except (VerificationError, InvalidHashError):
            return False
    if encoded.startswith("pbkdf2_sha256$"):
        return _check_pbkdf2(raw, encoded)
    return False


def needs_update(encoded: str) -> bool:
    """True when the hash is not argon2id with the current parameters."""
    match = _PARAMS.match(encoded)
    return match is None or tuple(map(int, match.groups())) != (MEMORY_KIB, TIME_COST, PARALLELISM)


def _check_pbkdf2(raw: str, encoded: str) -> bool:
    """Django format: `pbkdf2_sha256$<iterations>$<salt>$<base64 digest>`."""
    try:
        _, iterations, salt, digest = encoded.split("$")
        expected = base64.b64decode(digest, validate=True)
        if not _DIGITS.fullmatch(iterations) or int(iterations) < 1:
            return False
        actual = hashlib.pbkdf2_hmac("sha256", raw.encode(), salt.encode(), int(iterations))
    except (ValueError, OverflowError):
        return False
    return hmac.compare_digest(actual, expected)
