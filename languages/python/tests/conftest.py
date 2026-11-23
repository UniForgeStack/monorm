import asyncio
import os
from contextlib import contextmanager
from urllib.parse import quote, unquote, urlsplit

import pytest

from blog.models import Comment, Post, PostTag, Profile, Tag, User  # noqa: F401  (registers the models)

import monorm

DEFAULT_DATABASE_URL = "postgres://postgres:postgres@localhost/orm_test"


async def worker_database_url(base_url: str, worker: str) -> str:
    url = urlsplit(base_url)
    name = f"{unquote(url.path.removeprefix('/'))}_{worker}"
    if len(name.encode()) > 63:
        raise ValueError(f"PostgreSQL worker database name exceeds 63 bytes: {name!r}")

    database = await monorm.connect(
        base_url, default=False, registry=monorm.Registry(), max_connections=1
    )
    try:
        if not await database.fetch("SELECT 1 FROM pg_database WHERE datname::text = $1", name):
            identifier = name.replace('"', '""')
            await database.execute(f'CREATE DATABASE "{identifier}"')
    finally:
        await database.close()
    return url._replace(path="/" + quote(name, safe="")).geturl()


# Tests read these URLs during collection, before any fixture can run.
if worker := os.environ.get("PYTEST_XDIST_WORKER"):
    os.environ["ORM_TEST_DATABASE_URL"] = asyncio.run(
        worker_database_url(os.environ.get("ORM_TEST_DATABASE_URL", DEFAULT_DATABASE_URL), worker)
    )
    if docs_url := os.environ.get("ORM_DOCS_DATABASE_URL"):
        os.environ["ORM_DOCS_DATABASE_URL"] = asyncio.run(worker_database_url(docs_url, worker))

DATABASE_URL = os.environ.get("ORM_TEST_DATABASE_URL", DEFAULT_DATABASE_URL)


@pytest.fixture
def observe_queries():
    """Count executed statements separately from inspect()'s planned templates."""

    @contextmanager
    def observe(database):
        events: list[monorm.QueryEvent] = []
        remove = database.on_query(events.append)
        try:
            yield events
        finally:
            remove()

    return observe


@pytest.fixture(scope="session")
async def db():
    try:
        database = await monorm.connect(DATABASE_URL, max_connections=4)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    await database.drop_tables()
    await database.create_tables()
    yield database
    await database.drop_tables()
    await database.close()


@pytest.fixture
async def clean(db):
    yield db
    await db.execute("TRUNCATE post_tags, tags, profiles, comments, posts, users RESTART IDENTITY CASCADE")
