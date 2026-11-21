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
    provider_line = f'"{provider}"' + ('\n  bypass_roles = ["postgres"]' if provider == "postgresql" else "")
    models = monorm.loads(FIXTURE.replace('"sqlite"', provider_line), registry=registry)
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

        # versions mode: register, record twice, a repeated snapshot writes nothing, restore leaves a restore version
        BundleVersion = models["BundleVersion"]
        bundles = {1: {"id": 1, "title": "a"}}
        applied = []
        BundleVersion.history.register(generate=lambda id: dict(bundles[id]), restore=applied.append)
        versions = BundleVersion.history.using(db)
        first = await versions.record(1, "create")
        bundles[1]["title"] = "b"
        assert (await versions.record(1, "update")).version_number == 2
        assert (await versions.record(1, "update")).version_number == 2
        restored_version = await versions.restore(first)
        assert (restored_version.version_number, restored_version.change_type, restored_version.restored_from) == (3, "restore", first.id)
        assert applied == [{"id": 1, "title": "a"}], applied
        assert [e.event.version_number for e in await versions.timeline(1)] == [1, 2, 3]
    finally:
        await db.drop_tables()
        await db.close()


async def main() -> None:
    await run("sqlite", "sqlite://:memory:")
    if os.environ.get("ORM_TEST_DATABASE_URL"):
        await run("postgresql", os.environ["ORM_TEST_DATABASE_URL"])
    print("Python history extension: passed")


asyncio.run(main())
