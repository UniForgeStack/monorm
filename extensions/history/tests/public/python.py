"""`@@history.track` from the schema language to a timeline and a restore; needs the `monorm-history` package."""

import asyncio
import os
from datetime import datetime, timedelta, timezone
from pathlib import Path

import monorm
from monorm._packages import resolve_extensions

FIXTURE = Path(__file__).parent.parent.joinpath("fixtures/schema.prisma").read_text()
assert "monorm.history" in resolve_extensions(), "install the monorm-history package"


async def run(provider: str, url: str) -> None:
    registry = monorm.Registry()
    models = monorm.loads(FIXTURE.replace('"sqlite"', f'"{provider}"'), registry=registry)
    Post = models["Post"]
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        posts = Post.objects.using(db)
        post = await posts.insert(title="a", views=1)
        await posts.filter(Post.id == post.id).update(title="b", views=2)
        entries = await Post.history.using(db).timeline(post, diff=True)
        assert [e.event.label for e in entries] == ["insert", "update"], entries
        assert entries[1].diff == {"title": {"old": "a", "new": "b"}, "views": {"old": 1, "new": 2}}, entries[1].diff
        restored = await Post.history.using(db).restore(entries[0].event)
        assert (restored.title, restored.views) == ("a", 1)
        labels = [e.event.label for e in await Post.history.using(db).timeline(post)]
        assert labels == ["insert", "update", "restore"], labels
        await posts.filter(Post.id == post.id).delete()
        again = await Post.history.using(db).restore((await Post.history.using(db).timeline(post))[-1].event)
        assert (again.id, again.title) == (post.id, "a")
        later = datetime.now(timezone.utc) + timedelta(days=1)
        assert await Post.history.using(db).purge(before=later) == 5
    finally:
        await db.drop_tables()
        await db.close()


async def main() -> None:
    await run("sqlite", "sqlite://:memory:")
    if os.environ.get("ORM_TEST_DATABASE_URL"):
        await run("postgresql", os.environ["ORM_TEST_DATABASE_URL"])
    print("Python history extension: passed")


asyncio.run(main())
