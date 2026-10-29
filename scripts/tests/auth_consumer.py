"""Runs against models generated from `import "app:monorm-auth"` in a fresh package consumer."""
import asyncio
import os

import monorm
from models import User
from monorm_auth import UserMethods, UserQueries, authenticate, make_password


async def run() -> None:
    assert isinstance(User.objects, UserQueries) and issubclass(User, UserMethods)
    db = await monorm.connect(os.environ["MONORM_DATABASE_URL"])
    await db.drop_tables()
    await db.create_tables()
    try:
        await User.objects.using(db).insert(username="ann", email="ann@example.com", password=make_password("s3cret"))
        assert (await authenticate("ann", "s3cret", using=db)).username == "ann"
        assert await authenticate("ann", "wrong", using=db) is None
    finally:
        await db.drop_tables()
        await db.close()
    print("auth consumer (Python): passed")


asyncio.run(run())
