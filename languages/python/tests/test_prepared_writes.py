"""Prepared writes held in Rust: a call writes what the unprepared write writes."""

import os

import pytest
from blog.models import Comment, Post, User

import monorm
from artifact_caps import has_extension
from monorm import QueryError, param


async def users():
    return await User.objects.insert_many(
        [{"email": f"u{i}@example.com", "name": f"U{i}"} for i in range(3)]
    ).returning()


async def table():
    return [
        (p.title, p.body, p.views)
        for p in await Post.objects.order_by(Post.id)
    ]


async def test_prepared_insert_returns_the_instance_and_matches_the_plain_insert(clean):
    alice, *_ = await users()
    add = Post.objects.insert(author=alice, title=param("t"), body=param("b"), views=param("v")).prepare()
    first = await add(t="a", b="x", v=1)
    second = await add(t="b", b="y", v=2)
    plain = await Post.objects.insert(author=alice, title="c", body="z", views=3)
    assert (first.title, second.title, plain.title) == ("a", "b", "c")
    assert first.id != second.id
    assert await table() == [("a", "x", 1), ("b", "y", 2), ("c", "z", 3)]


async def test_prepared_insert_inspect_equals_the_plain_inspect(clean):
    alice, *_ = await users()
    add = Post.objects.insert(author=alice, title=param("t"), body=param("b")).prepare()
    plain = Post.objects.insert(author=alice, title="a", body="x")
    assert add.inspect(t="a", b="x") == plain.inspect()


async def test_a_param_write_needs_prepare(clean):
    alice, *_ = await users()
    with pytest.raises(QueryError, match="prepare"):
        await Post.objects.insert(author=alice, title=param("t"), body="")
    with pytest.raises(QueryError, match="prepare"):
        await Post.objects.filter(Post.id == param("i")).update(title="x")
    with pytest.raises(QueryError, match="prepare"):
        await Post.objects.filter(Post.id == 1).update(title=param("t"))


async def test_prepared_insert_many_batches(clean):
    alice, *_ = await users()
    add = Post.objects.insert_many(param("rows", fields=(Post.author_id, Post.title, Post.body))).prepare()
    for n in (0, 1, 31, 32, 33, 40000):
        await Post.objects.all().delete()
        rows = [{"author_id": alice.id, "title": f"t{i}", "body": ""} for i in range(n)]
        assert await add(rows=rows) == n
        assert await Post.objects.count() == n


async def test_a_large_prepared_batch_keeps_row_order_with_returning_and_upsert(clean):
    alice, *_ = await users()
    add = Post.objects.insert_many(param("rows", fields=(Post.author_id, Post.title, Post.body))).returning().prepare()
    rows = [{"author_id": alice.id, "title": f"t{i}", "body": ""} for i in range(40)]
    got = await add(rows=rows)
    assert [p.title for p in got] == [f"t{i}" for i in range(40)]
    assert len({p.id for p in got}) == 40
    mail = User.objects.insert_many(param("rows", fields=(User.email, User.name))).on_conflict(User.email, update=True).prepare()
    people = [{"email": f"m{i}@example.com", "name": "a"} for i in range(40)]
    assert await mail(rows=people) == 40
    assert await mail(rows=[{**r, "name": "b"} for r in people]) == 40
    assert {u.name for u in await User.objects.filter(User.email.like("m%"))} == {"b"}


async def test_prepared_update_and_delete_match_the_plain_write(clean):
    alice, *_ = await users()
    posts = await Post.objects.insert_many(
        [{"author": alice, "title": f"t{i}", "body": "", "views": i} for i in range(3)]
    ).returning()
    ids = [p.id for p in posts]
    edit = Post.objects.filter(Post.id == param("id")).update(title=param("t")).prepare()
    assert edit.inspect(id=ids[0], t="x") == Post.objects.filter(Post.id == ids[0]).update(title="x").inspect()
    assert await edit(id=ids[0], t="x") == 1
    assert await edit(id=-1, t="x") == 0
    shown = Post.objects.filter(Post.id == param("id")).update(views=param("v")).returning(Post.id, Post.views).prepare()
    assert [(r.id, r.views) for r in await shown(id=ids[1], v=9)] == [(ids[1], 9)]
    drop = Post.objects.filter(Post.id == param("id")).delete().prepare()
    assert await drop(id=ids[2]) == 1
    assert await drop(id=ids[2]) == 0
    assert await table() == [("x", "", 0), ("t1", "", 9)]


async def test_none_is_null_in_a_value_and_an_error_in_a_filter(clean):
    alice, *_ = await users()
    add = Post.objects.insert(author=alice, title="a", body=param("b")).prepare()
    post = await add(b="x")
    comments = Comment.objects.insert(post_id=post.id, body="c", author_id=param("a")).prepare()
    assert (await comments(a=None)).author_id is None
    assert (await comments(a=alice.id)).author_id == alice.id
    edit = Post.objects.filter(Post.id == param("id")).update(body=param("b")).prepare()
    with pytest.raises(QueryError):
        await edit(id=None, b="y")


async def test_none_is_null_in_a_set_expression(clean):
    alice, *_ = await users()
    post = await Post.objects.insert(author=alice, title="a", body="")
    comment = await Comment.objects.insert(post_id=post.id, body="c", author_id=alice.id)
    shift = Comment.objects.filter(Comment.id == param("id")).update(author_id=Comment.author_id + param("n", int)).prepare()
    assert await shift(id=comment.id, n=0) == 1
    assert await shift(id=comment.id, n=None) == 1
    assert (await Comment.objects.get(Comment.id == comment.id)).author_id is None


async def test_none_in_an_update_many_filter_param_is_an_error(clean):
    alice, *_ = await users()
    posts = await Post.objects.insert_many([{"author": alice, "title": "a", "body": ""} for _ in range(40)]).returning()
    edit = Post.objects.filter(Post.views == param("v")).update_many(param("rows", fields=(Post.id, Post.title))).prepare()
    rows = [{"id": p.id, "title": "b"} for p in posts]
    assert await edit(v=0, rows=rows[:1]) == 1
    for batch in (rows[:1], rows):
        with pytest.raises(QueryError):
            await edit(v=None, rows=batch)
    assert [p.title for p in await Post.objects.order_by(Post.id)].count("b") == 1


async def test_a_row_with_a_missing_or_an_extra_key_fails_before_any_sql(clean):
    alice, *_ = await users()
    add = Post.objects.insert_many(param("rows", fields=(Post.author_id, Post.title, Post.body))).prepare()
    good = {"author_id": alice.id, "title": "t", "body": ""}
    with pytest.raises(ValueError, match="no body"):
        await add(rows=[good, {"author_id": alice.id, "title": "t"}])
    with pytest.raises(ValueError, match="extra"):
        await add(rows=[good, {**good, "extra": 1}])
    assert await Post.objects.count() == 0


async def test_prepared_update_many_matches_the_plain_update_many(clean):
    alice, *_ = await users()
    posts = await Post.objects.insert_many(
        [{"author": alice, "title": f"t{i}", "body": ""} for i in range(40)]
    ).returning()
    edit = Post.objects.update_many(param("rows", fields=(Post.id, Post.title))).prepare()
    for n in (0, 1, 31, 32, 40):
        rows = [{"id": p.id, "title": f"n{n}-{p.id}"} for p in posts[:n]]
        assert await edit(rows=rows) == n
    assert (await Post.objects.get(Post.id == posts[39].id)).title == f"n40-{posts[39].id}"
    with pytest.raises(ValueError):
        await edit(rows=[{"title": "x"}])


async def test_prepared_upsert_keeps_its_conflict_fields(clean):
    alice, *_ = await users()
    add = (
        User.objects.insert(email=param("e"), name=param("n"))
        .on_conflict(User.email, update=False)
        .prepare()
    )
    assert await add(e=alice.email, n="other") == 0
    assert await add(e="new@example.com", n="N") == 1
    assert (await User.objects.get(User.id == alice.id)).name == alice.name
    upd = User.objects.insert(email=param("e"), name=param("n")).on_conflict(User.email, update=True).prepare()
    await upd(e=alice.email, n="renamed")
    assert (await User.objects.get(User.id == alice.id)).name == "renamed"


# -- schemas of their own ----------------------------------------------------------------------

SCHEMA = """
model Item {
 shop Int
 id Int
 name String
 tags String[]
 mail String @db.Citext
 @@id([shop, id])
 @@map("pw_item")
}
model Note {
 id Int @id @default(autoincrement())
 shop Int
 name String
 views Int @default(0)
 @@query.filter("tenant", "shop == scope.shop")
 @@map("pw_note")
}
model Vault {
 id Int @id
 name String
 @@protected_write
 @@map("pw_vault")
}
"""


@pytest.fixture
async def pg():
    url = os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads('datasource db {\n provider = "postgresql"\n}\n' + SCHEMA, registry=registry)
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    yield db, models
    await db.drop_tables()
    await db.close()


async def test_composite_keys_array_columns_and_write_sql_fields_in_batches(pg):
    db, models = pg
    Item = models["Item"].objects.using(db)
    model = models["Item"]
    add = Item.insert_many(param("rows", fields=(model.shop, model.id, model.name, model.tags, model.mail))).prepare()
    for n in (1, 31, 32, 33, 70):
        await Item.all().delete()
        rows = [
            {"shop": i % 3, "id": i, "name": f"n{i}", "tags": [f"a{i}", "b"], "mail": f"M{i}@X.com"} for i in range(n)
        ]
        assert await add(rows=rows) == n
        got = await Item.order_by(model.id)
        assert [(r.shop, r.id, r.tags, r.mail) for r in got] == [(r["shop"], r["id"], r["tags"], r["mail"]) for r in rows]
    edit = Item.update_many(param("rows", fields=(model.shop, model.id, model.name, model.tags))).prepare()
    rows = [{"shop": i % 3, "id": i, "name": f"x{i}", "tags": [f"z{i}"]} for i in range(70)]
    assert await edit(rows=rows) == 70
    got = await Item.order_by(model.id)
    assert [(r.name, r.tags) for r in got] == [(r["name"], r["tags"]) for r in rows]
    assert await edit(rows=[{**rows[0], "shop": 99}]) == 0


async def test_scope_binds_per_call_in_a_prepared_write(pg):
    db, models = pg
    Note = models["Note"]
    notes = Note.objects.using(db)
    await notes.without_defaults().insert_many([{"shop": s, "name": "n", "views": 0} for s in (1, 1, 2)])
    bump = notes.filter(Note.name == param("n")).update(views=param("v")).prepare()
    with monorm.scope(shop=1):
        assert await bump(n="n", v=5) == 2
    assert await bump(n="n", v=7, scope={"shop": 2}) == 1
    got = sorted((r.shop, r.views) for r in await notes.without_defaults().all())
    assert got == [(1, 5), (1, 5), (2, 7)]
    drop = notes.filter(Note.name == param("n")).delete().prepare()
    assert await drop(n="n", scope={"shop": 1}) == 2
    assert await notes.without_defaults().count() == 1


async def test_a_scoped_update_many_of_33_rows_keeps_the_scope_filter(pg):
    db, models = pg
    Note = models["Note"]
    notes = Note.objects.using(db)
    await notes.without_defaults().insert_many([{"shop": 1 + i % 2, "name": f"n{i}", "views": 0} for i in range(40)])
    ids = [(r.id, r.shop) for r in await notes.without_defaults().order_by(Note.id)]
    edit = notes.update_many(param("rows", fields=(Note.id, Note.views))).prepare()
    rows = [{"id": id_, "views": 100 + i} for i, (id_, _) in enumerate(ids[:33])]
    with monorm.scope(shop=1):
        assert await edit(rows=rows) == 17
    got = {r.id: (r.shop, r.views) for r in await notes.without_defaults().all()}
    for i, (id_, shop) in enumerate(ids):
        assert got[id_] == (shop, 100 + i if i < 33 and shop == 1 else 0)


async def test_the_write_guard_and_protected_write_run_per_call(pg):
    db, models = pg
    Vault = models["Vault"]
    vaults = Vault.objects.using(db)
    add = vaults.insert(id=param("i"), name=param("n")).prepare()
    with pytest.raises(monorm.WriteProtected):
        await add(i=1, n="a")
    with monorm.allow_writes(Vault):
        await add(i=1, n="a")
    with pytest.raises(monorm.WriteProtected):
        await add(i=2, n="b")
    assert await vaults.count() == 1
    rename = vaults.filter(Vault.id == param("i")).update(name=param("n")).prepare()
    with pytest.raises(monorm.WriteProtected):
        await rename(i=1, n="c")
    with monorm.allow_writes(Vault):
        assert await rename(i=1, n="c") == 1
    with pytest.raises(monorm.UnfilteredWriteError):
        await vaults.update(name="x").prepare()
    assert (await vaults.get(Vault.id == 1)).name == "c"


@pytest.mark.skipif(not has_extension("monorm.timestamps"), reason="requires monorm.timestamps")
async def test_timestamp_hooks_run_per_call(pg):
    registry = monorm.Registry()
    models = monorm.loads(
        'datasource db {\n provider = "postgresql"\n}\n'
        'model Stamp {\n id Int @id\n views Int @default(0)\n'
        ' @@timestamps.updated_at("changed", mode: "application")\n @@map("pw_stamp")\n}\n',
        registry=registry,
    )
    db = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        Stamp = models["Stamp"]
        stamps = Stamp.objects.using(db)
        add = stamps.insert(id=param("i")).prepare()
        first = await add(i=1)
        second = await add(i=2)
        assert second.changed >= first.changed
        edit = stamps.filter(Stamp.id == param("i")).update(views=param("v")).prepare()
        before = (await stamps.get(Stamp.id == 1)).changed
        await edit(i=1, v=3)
        assert (await stamps.get(Stamp.id == 1)).changed > before
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not has_extension("monorm.composition"), reason="requires monorm.composition")
async def test_a_multi_table_insert_runs_the_same_statements_as_the_plain_insert(pg, observe_queries):
    db, _ = pg
    registry = monorm.Registry()
    models = monorm.loads(
        'datasource db {\n provider = "postgresql"\n}\n'
        'model Person {\n id Int @id @default(autoincrement())\n name String\n @@map("pw_people")\n}\n'
        'model Staff {\n salary Int\n'
        ' @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")\n @@map("pw_staff")\n}\n',
        registry=registry,
    )
    db = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        staff = models["Staff"].objects.using(db)
        with observe_queries(db) as plain:
            await staff.insert(name="a", salary=1)
        add = staff.insert(name=param("n"), salary=param("s")).prepare()
        with observe_queries(db) as events:
            row = await add(n="b", s=2)
        assert [e.sql for e in events] == [e.sql for e in plain]
        assert (row.name, row.salary) == ("b", 2)
        assert sorted((r.name, r.salary) for r in await staff.all()) == [("a", 1), ("b", 2)]
    finally:
        await db.drop_tables()
        await db.close()
