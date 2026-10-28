import os
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

import pytest

import monorm
from auth_support import VARIANTS, Auth
from monorm_auth import UserMethods, UserQueries

POSTGRES_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
HEADER = 'datasource db {\n  provider = "%s"\n}\nimport "app:monorm-auth" (prefix: "auth_")\n'


@pytest.fixture(params=["sqlite", "postgres"])
def provider(request: pytest.FixtureRequest) -> str:
    return str(request.param)


@pytest.fixture(params=list(VARIANTS))
def variant(request: pytest.FixtureRequest) -> str:
    return str(request.param)


@pytest.fixture
async def make_auth(tmp_path: Path, provider: str, variant: str) -> AsyncIterator[Callable[[], Any]]:
    opened: list[monorm.Database] = []

    async def make() -> Auth:
        registry = monorm.Registry()
        schema = tmp_path / "schema.prisma"
        kind = VARIANTS[variant]
        schema.write_text(HEADER % provider + kind.schema)
        models = monorm.load(schema, registry=registry, model_methods={kind.model: UserMethods})
        monorm.use_query_set(models[kind.model], UserQueries)
        url = POSTGRES_URL if provider == "postgres" else "sqlite://:memory:"
        try:
            db = await monorm.connect(url, registry=registry, default=False)
        except monorm.DatabaseError as e:
            if "ORM_TEST_DATABASE_URL" in os.environ:
                raise
            pytest.skip(f"Postgres not reachable at {url}: {e}")
        opened.append(db)
        await db.drop_tables()
        await db.create_tables()
        return Auth(db, registry, models, kind)

    yield make
    for db in opened:
        await db.drop_tables()
        await db.close()


@pytest.fixture
async def auth(make_auth: Callable[[], Any]) -> Auth:
    result: Auth = await make_auth()
    return result
