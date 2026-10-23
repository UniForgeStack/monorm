import json
from pathlib import Path

import pytest

from orm_auth import check_password, make_password, needs_update

VECTORS = json.loads(Path(__file__).parents[2].joinpath("fixtures/hashes.json").read_text())


def test_make_password_gives_an_argon2id_hash_with_a_new_salt_each_time() -> None:
    a, b = make_password("s3cret"), make_password("s3cret")
    assert a.startswith("$argon2id$v=19$m=65536,t=3,p=4$") and a != b
    assert check_password("s3cret", a) and not check_password("other", a)
    assert not needs_update(a)


@pytest.mark.parametrize("case", VECTORS["pbkdf2"])
def test_django_pbkdf2_vectors(case: dict[str, object]) -> None:
    assert check_password(str(case["raw"]), str(case["encoded"])) is case["valid"]


@pytest.mark.parametrize("case", VECTORS["argon2id"])
def test_argon2id_vectors(case: dict[str, object]) -> None:
    assert check_password(str(case["raw"]), str(case["encoded"])) is case["valid"]
    assert needs_update(str(case["encoded"])) is case["needs_update"]


@pytest.mark.parametrize("encoded", ["", "!", "$argon2id$broken", "plain", "bcrypt$x"])
def test_an_unknown_or_broken_hash_is_not_a_match(encoded: str) -> None:
    assert check_password("s3cret", encoded) is False


def test_a_pbkdf2_hash_needs_an_update() -> None:
    assert needs_update(VECTORS["pbkdf2"][0]["encoded"])
