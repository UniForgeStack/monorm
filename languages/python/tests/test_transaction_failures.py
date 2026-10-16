"""A failed statement marks its transaction failed, also when the block catches the error."""
from types import SimpleNamespace

import pytest

import orm
from blog.models import User
from test_sqlite import models


@pytest.fixture(params=["postgres", "sqlite"])
async def case(request, clean):
    if request.param == "postgres":
        yield SimpleNamespace(
            db=clean, model=User, make=lambda i: {"email": f"{i}@x.io", "name": "n"}
        )
        return
    registry, classes = models()
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield SimpleNamespace(db=db, model=classes["Author"], make=lambda i: {"email": f"{i}@x.io", "name": "n"})
    await db.drop_tables()
    await db.close()


async def count(c):
    return await c.model.objects.using(c.db).count()


async def seed(c):
    await c.model.objects.using(c.db).insert(**c.make("dup"))


async def test_a_caught_error_aborts_the_commit(case):
    c, seen = case, []
    await seed(c)
    with pytest.raises(orm.TransactionAborted) as e:
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("kept"))
            with pytest.raises(orm.IntegrityError):
                await seed(c)
            await c.db.on_commit(lambda: seen.append("ran"))
    assert isinstance(e.value.__cause__, orm.IntegrityError)
    assert e.value.__cause__.sqlstate == "23505" and "earlier error" in str(e.value)
    assert str(e.value.__cause__).splitlines()[0] in str(e.value)
    assert isinstance(e.value, orm.DatabaseError)
    assert await count(c) == 1 and seen == []


async def test_a_later_statement_raises_transaction_aborted(case):
    c = case
    await seed(c)
    with pytest.raises(orm.IntegrityError):
        async with c.db.transaction():
            with pytest.raises(orm.IntegrityError) as first:
                await seed(c)
            with pytest.raises(ValueError):
                async with c.db.transaction(savepoint=False):
                    raise ValueError("a later application failure")
            with pytest.raises(orm.TransactionAborted) as later:
                await c.model.objects.using(c.db).insert(**c.make("later"))
            assert later.value.__cause__ is not first.value
            assert isinstance(later.value.__cause__, orm.IntegrityError)
            with pytest.raises(orm.TransactionAborted):
                await count(c)
            raise first.value
    assert await count(c) == 1


async def test_a_savepoint_in_a_try_keeps_the_other_writes(case):
    c, seen = case, []
    await seed(c)
    async with c.db.transaction():
        await c.model.objects.using(c.db).insert(**c.make("before"))
        with pytest.raises(orm.IntegrityError):
            async with c.db.transaction():
                await c.db.on_commit(lambda: seen.append("inner"))
                await seed(c)
        await c.model.objects.using(c.db).insert(**c.make("after"))
        await c.db.on_commit(lambda: seen.append("outer"))
    assert await count(c) == 3 and seen == ["outer"]


async def test_a_caught_error_in_a_savepoint_block_aborts_that_savepoint(case):
    c = case
    await seed(c)
    async with c.db.transaction():
        with pytest.raises(orm.TransactionAborted):
            async with c.db.transaction():
                with pytest.raises(orm.IntegrityError):
                    await seed(c)
        await c.model.objects.using(c.db).insert(**c.make("after"))
    assert await count(c) == 2


async def test_savepoint_false_joins_the_enclosing_transaction(case):
    c = case
    await seed(c)
    async with c.db.transaction():
        async with c.db.transaction(savepoint=False):
            await c.model.objects.using(c.db).insert(**c.make("joined"))
    assert await count(c) == 2
    with pytest.raises(orm.TransactionAborted):
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("lost"))
            with pytest.raises(orm.IntegrityError):
                async with c.db.transaction(savepoint=False):
                    await seed(c)
    assert await count(c) == 2
    with pytest.raises(orm.TransactionAborted):
        async with c.db.transaction():
            with pytest.raises(ValueError):
                async with c.db.transaction(savepoint=False):
                    raise ValueError("not a database error")
    assert await count(c) == 2
    async with c.db.transaction(savepoint=False):  # no effect on an outermost block
        await c.model.objects.using(c.db).insert(**c.make("outer"))
    assert await count(c) == 3


async def test_an_uncaught_error_rolls_back_and_raises_the_original(case):
    c = case
    await seed(c)
    with pytest.raises(orm.IntegrityError) as e:
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("lost"))
            await seed(c)
    assert not isinstance(e.value, orm.TransactionAborted)
    assert await count(c) == 1


async def test_joined_application_error_keeps_original_cause(case):
    c = case
    original = ValueError("application failure")
    with pytest.raises(orm.TransactionAborted) as commit:
        async with c.db.transaction():
            await c.model.objects.using(c.db).insert(**c.make("lost"))
            with pytest.raises(ValueError):
                async with c.db.transaction(savepoint=False):
                    raise original
            # A second failure must not replace the first exception.
            with pytest.raises(TypeError):
                async with c.db.transaction(savepoint=False):
                    raise TypeError("second failure")
            with pytest.raises(orm.TransactionAborted) as later:
                await count(c)
            assert later.value.__cause__ is original
            with pytest.raises(orm.TransactionAborted) as begin:
                async with c.db.transaction():
                    pass
            assert begin.value.__cause__ is original
    assert commit.value.__cause__ is original
    assert await count(c) == 0


@pytest.mark.parametrize("application_error", [False, True])
async def test_joined_failure_in_savepoint_leaves_outer_usable(case, application_error):
    c, seen = case, []
    await seed(c)
    async with c.db.transaction():
        await c.model.objects.using(c.db).insert(**c.make("before"))
        with pytest.raises(orm.TransactionAborted):
            async with c.db.transaction():
                await c.db.on_commit(lambda: seen.append("inner"))
                await c.model.objects.using(c.db).insert(**c.make("lost"))
                with pytest.raises(ValueError if application_error else orm.IntegrityError):
                    async with c.db.transaction(savepoint=False):
                        if application_error:
                            raise ValueError("joined savepoint failed")
                        await seed(c)
        await c.model.objects.using(c.db).insert(**c.make("after"))
        await c.db.on_commit(lambda: seen.append("outer"))
    assert await count(c) == 3 and seen == ["outer"]


@pytest.mark.parametrize("application_error", [False, True])
async def test_caught_data_migration_failure_rolls_back(case, tmp_path, application_error):
    from orm.migrations import Migration, Migrations, Migrator

    c, seen = case, []
    path = tmp_path / "0001_failure"
    path.mkdir()
    (path / "up.sql").write_text("CREATE TABLE migration_probe (id INTEGER)")
    (path / "down.sql").write_text("DROP TABLE migration_probe")
    (path / "snapshot.json").write_text(c.db._registry.prepare().snapshot())
    migration = Migration(path.name, path)
    original = ValueError("data migration failed")

    async def run(db):
        await db.execute("INSERT INTO migration_probe VALUES (1)")
        await c.model.objects.using(db).insert(**c.make("lost"))
        if application_error:
            with pytest.raises(ValueError):
                async with db.transaction(savepoint=False):
                    raise original
        else:
            with pytest.raises(orm.IntegrityError):
                await seed(c)
        await db.on_commit(lambda: seen.append("ran"))

    await seed(c)
    migrator = Migrator(c.db, Migrations(tmp_path, c.db._registry))
    await migrator._load_pending(None)
    with pytest.raises(orm.TransactionAborted) as aborted:
        await migrator._upgrade(None, [(migration, run)])
    if application_error:
        assert aborted.value.__cause__ is original
    else:
        assert isinstance(aborted.value.__cause__, orm.IntegrityError)
    assert await count(c) == 1 and seen == []
    with pytest.raises(orm.DatabaseError):
        await c.db.fetch("SELECT * FROM migration_probe")
    assert not await c.db.fetch("SELECT * FROM orm_migrations")
    await c.db.execute("DROP TABLE orm_migrations")


# -- a failure in an ORM-internal multi-statement call marks the enclosing transaction -------------

FOLLOW = """
datasource db {
  provider = "sqlite"
}

model Person {
  id        BigInt   @id @default(autoincrement())
  name      String
  following Person[] @relation(through: Follow, through_fields: [follower, followee])
}

model Follow {
  id          BigInt @id @default(autoincrement())
  follower_id BigInt
  followee_id BigInt
  follower    Person @relation("follower", fields: [follower_id], references: [id])
  followee    Person @relation("followee", fields: [followee_id], references: [id])

  @@unique([follower_id, followee_id])
}
"""


@pytest.fixture(params=["postgres", "sqlite"])
async def linked(request, clean):
    """`link(db)` gives (a person that follows one, a key that fails to link, the follow count)."""
    if request.param == "postgres":
        from blog.models import Post, PostTag, Tag

        async def link():
            alice = await User.objects.insert(email="a@x.io", name="A")
            post = await Post.objects.insert(author=alice, title="t", body="b")
            t1, t2 = await Tag.objects.insert_many([{"name": "a"}, {"name": "b"}]).returning()
            await post.tags.add(t1)
            return SimpleNamespace(rel=post.tags, bad=t2.id + 10_000, kept=t1.id, count=PostTag.objects.count,
                                   links=lambda: PostTag.objects.all(), key="tag_id", db=clean,
                                   fail={})

        yield link
        return
    from test_sqlite import models as sqlite_models

    registry, m = sqlite_models(FOLLOW)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    Person, Follow = m["Person"], m["Follow"]

    async def link():
        ann, bob, cat = await Person.objects.using(db).insert_many([{"name": "a"}, {"name": "b"}, {"name": "c"}]).returning()
        await ann.following.using(db).add(bob)
        return SimpleNamespace(rel=ann.following.using(db), bad=cat.id + 1000, kept=bob.id,
                               count=Follow.objects.using(db).count, links=lambda: Follow.objects.using(db).all(),
                               key="followee_id", db=db, fail={})

    yield link
    await db.drop_tables()
    await db.close()


async def test_a_failed_related_set_marks_the_enclosing_transaction(linked):
    c = await linked()
    with pytest.raises(orm.TransactionAborted) as e:
        async with c.db.transaction():
            with pytest.raises(orm.DatabaseError):
                await c.rel.set([c.bad], **c.fail)
            await c.db.on_commit(lambda: None)
    assert isinstance(e.value.__cause__, orm.DatabaseError)
    assert await c.count() == 1


async def test_a_failed_related_set_outside_a_transaction_stays_atomic(linked):
    c = await linked()
    with pytest.raises(orm.DatabaseError):
        await c.rel.set([c.bad], **c.fail)
    assert [getattr(link, c.key) for link in await c.links()] == [c.kept]


async def test_a_nested_transaction_recovers_a_failed_related_set(linked):
    c = await linked()
    async with c.db.transaction():
        with pytest.raises(orm.DatabaseError):
            async with c.db.transaction():
                await c.rel.set([c.bad], **c.fail)
        assert await c.count() == 1
    assert [getattr(link, c.key) for link in await c.links()] == [c.kept]
