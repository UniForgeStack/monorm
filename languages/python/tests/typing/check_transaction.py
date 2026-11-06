"""Static typing checks for the transaction options; run by languages/python/tests/test_typing.py
with mypy and pyright. Never executed."""

from typing import Literal, assert_type

from monorm import DatabaseError, Isolation, QueryError, TransactionTimeoutError
from monorm.db import Database


async def options(db: Database) -> None:
    async with db.transaction(isolation="serializable", timeout=2.5):
        pass
    async with db.transaction(isolation=None, timeout=None, savepoint=False):
        pass
    async with db.transaction(timeout=1):  # an int is a number
        pass
    async with db.transaction(isolation="snapshot"):  # E: not an isolation level
        pass
    async with db.transaction(timeout="1"):  # E: a timeout is a number
        pass


def levels(level: Isolation) -> None:
    assert_type(level, Isolation)


def errors(e: TransactionTimeoutError) -> None:
    assert_type(e.outcome, Literal["rolled_back", "unknown"] | None)
    assert isinstance(e, DatabaseError)
    assert not isinstance(e, QueryError)

