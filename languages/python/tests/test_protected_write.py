"""`@@write_rules.protect(layer: app)` and `monorm.allow_writes`: an application-level write check."""

import asyncio

import pytest
import monorm
from artifact_caps import has_extension


SOURCE = """datasource db {
  provider = "sqlite"
}
model Post {
  id    Int       @id
  title String
  tags  Tag[]     @relation(through: PostTag)
  links PostTag[]
  @@write_rules.protect(layer: app)
}
model Tag {
  id    Int       @id
  name  String
  posts Post[]    @relation(through: PostTag)
  links PostTag[]
}
model PostTag {
  id      Int  @id @default(autoincrement())
  post_id Int
  tag_id  Int
  post    Post @relation(fields: [post_id], references: [id])
  tag     Tag  @relation(fields: [tag_id], references: [id])
  @@unique([post_id, tag_id])
  @@write_rules.protect(layer: app)
}
"""


@pytest.fixture
async def blog():
    registry = monorm.Registry()
    models = monorm.loads(SOURCE, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield models, db
    await db.close()


async def test_every_write_method_fails_outside_allow_writes_and_works_inside(blog):
    models, db = blog
    Post = models["Post"]
    posts = Post.objects.using(db)
    writes = [
        lambda: posts.insert(id=1, title="a"),
        lambda: posts.insert_many([{"id": 2, "title": "b"}, {"id": 3, "title": "c"}]),
        lambda: posts.insert(id=1, title="upsert").on_conflict(Post.id, update=True),
        lambda: posts.filter(Post.id == 1).update(title="x"),
        lambda: posts.update_many([{"id": 2, "title": "y"}]),
        lambda: posts.filter(Post.id == 3).delete(),
    ]
    for write in writes:
        with pytest.raises(monorm.WriteProtected, match=r"Post is write-protected.*monorm.allow_writes\(Post\)"):
            await write()
        with monorm.allow_writes(Post):
            await write()
    assert [(p.id, p.title) for p in await posts.order_by(Post.id)] == [(1, "x"), (2, "y")]
    post = await posts.get(Post.id == 1)
    with pytest.raises(monorm.WriteProtected):
        await post.update(title="z")
    with pytest.raises(monorm.WriteProtected):
        await post.delete()
    with monorm.allow_writes(Post):
        await post.update(title="z")
        await post.delete()
    assert await posts.count() == 1


async def test_the_check_is_on_the_model_the_sql_writes(blog):
    models, db = blog
    Post, Tag, PostTag = models["Post"], models["Tag"], models["PostTag"]
    with monorm.allow_writes(Post):
        post = await Post.objects.using(db).insert(id=1, title="a")
    tag = await Tag.objects.using(db).insert(id=1, name="t")  # not protected
    # post.tags.add() writes PostTag rows, so the protection of PostTag decides.
    with monorm.allow_writes(Post), pytest.raises(monorm.WriteProtected, match="PostTag"):
        await post.tags.add(tag)
    with monorm.allow_writes(PostTag):
        await post.tags.add(tag)
    assert await PostTag.objects.using(db).count() == 1
    # Scopes nest: an inner scope adds to the models of the outer one.
    with monorm.allow_writes(Post), monorm.allow_writes(Tag):
        await Post.objects.using(db).filter(Post.id == 1).update(title="b")
    with pytest.raises(TypeError, match="model class"):
        with monorm.allow_writes("Post"):  # type: ignore[arg-type]
            pass
    with pytest.raises(TypeError, match="model class"):
        with monorm.allow_writes(post):
            pass


async def test_a_package_check_before_its_own_io_also_checks_protection(blog):
    from monorm.hooks import prepare_insert, prepare_update

    models, db = blog
    Post = models["Post"]
    # A file field uploads after these checks, so a rejected write uploads nothing.
    with pytest.raises(monorm.WriteProtected, match="Post"):
        prepare_insert(Post.objects.using(db), {"id": 1, "title": "a"})
    with pytest.raises(monorm.WriteProtected, match="Post"):
        prepare_update(Post.objects.using(db).filter(Post.id == 1), {"title": "b"})
    with monorm.allow_writes(Post):
        prepare_insert(Post.objects.using(db), {"id": 1, "title": "a"})
        assert prepare_update(Post.objects.using(db).filter(Post.id == 1), {"title": "b"}).unique


async def test_the_scope_follows_async_work_and_reads_and_raw_sql_are_unchanged(blog):
    models, db = blog
    Post = models["Post"]

    async def insert(id):
        return await Post.objects.using(db).insert(id=id, title="a")

    with monorm.allow_writes(Post):
        task = asyncio.create_task(insert(1))
    await task
    later = asyncio.create_task(insert(2))
    with pytest.raises(monorm.WriteProtected):
        await later
    # A lazy write reads the scope at await, not where it was built.
    with monorm.allow_writes(Post):
        lazy = Post.objects.using(db).insert(id=3, title="a")
    with pytest.raises(monorm.WriteProtected):
        await lazy
    assert await Post.objects.using(db).count() == 1
    assert await db.execute("UPDATE post SET title = 'raw'") == 1
    assert (await Post.objects.using(db).get()).title == "raw"


def test_protected_write_creates_no_ddl_change():
    def ddl(source):
        registry = monorm.Registry()
        monorm.loads(source, registry=registry)
        schema = registry.prepare()
        return schema.ddl()

    assert ddl(SOURCE) == ddl(SOURCE.replace("@@write_rules.protect(layer: app)", ""))
    with pytest.raises(monorm.SchemaError, match=r"@@write_rules\.protect\(layer: app\)"):
        monorm.loads(SOURCE.replace("@@write_rules.protect(layer: app)", "@@protected_write", 1), registry=monorm.Registry())


@pytest.mark.skipif(not has_extension("monorm.proxy"), reason="requires the monorm-proxy package")
async def test_a_proxy_writes_the_table_of_its_protected_root():
    source = SOURCE + "model Draft {\n  @@proxy.of(Post)\n}\n"
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    Post, Draft = models["Post"], models["Draft"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(monorm.WriteProtected, match="Post"):
            await Draft.objects.using(db).insert(id=1, title="a")
        with monorm.allow_writes(Post):
            await Draft.objects.using(db).insert(id=1, title="a")
        with monorm.allow_writes(Draft):
            await Draft.objects.using(db).filter(Draft.id == 1).delete()
    finally:
        await db.close()
    # The proxy shares the protection of its root, so it cannot declare its own.
    with pytest.raises(
        monorm.SchemaError, match="Draft: a proxy cannot declare a write rule; protect the root model Post"
    ):
        monorm.loads(
            SOURCE.replace("  @@write_rules.protect(layer: app)\n", "", 1)
            + "model Draft {\n  @@proxy.of(Post)\n  @@write_rules.protect(layer: app)\n}\n",
            registry=monorm.Registry(),
        )


@pytest.mark.skipif(not has_extension("monorm.composition"), reason="requires the monorm.composition package")
async def test_a_composed_write_also_writes_its_protected_parent():
    source = """datasource db {
  provider = "sqlite"
}
model Person {
  id   Int    @id @default(autoincrement())
  name String
  @@write_rules.protect(layer: app)
}
model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}
"""
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    Person, Employee = models["Person"], models["Employee"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(monorm.WriteProtected, match="Person"):
            await Employee.objects.using(db).insert(name="a", salary=1)
        with monorm.allow_writes(Employee, Person):
            alice = await Employee.objects.using(db).insert(name="a", salary=1)
        with pytest.raises(monorm.WriteProtected, match="Person"):
            await Employee.objects.using(db).filter(Employee.id == alice.id).update(name="b")
        with monorm.allow_writes(Person):
            bob = await Person.objects.using(db).insert(name="b")
        with pytest.raises(monorm.WriteProtected):
            await Employee.objects.using(db).attach(bob.id, {"salary": 2})
        with monorm.allow_writes(Employee, Person):
            await Employee.objects.using(db).attach(bob.id, {"salary": 2})
    finally:
        await db.close()


RULES = """
model Ledger {
  id Int @id
  note String
  slug String @write_rules.read_only
  @@map("wr_py_ledger")
  @@write_rules.protect(operation: [delete])
  @@write_rules.protect(operation: [update], condition: "OLD.note = 'locked'", layer: db)
}
model Dropped {
  id Int @id
  @@map("wr_py_dropped_table_with_a_long_name_for_introspection")
  @@write_rules.protect(operation: [insert], layer: db, action: drop)
}
model Openable {
  id Int @id
  @@map("wr_py_openable")
  @@write_rules.protect(operation: [insert], skippable: true)
}
model Frozen {
  id Int @id
  @@map("wr_py_frozen")
  @@write_rules.protect
}
model Base {
  @@inherit.abstract
  @@write_rules.protect(operation: [update])
}
model Child {
  id Int @id
  note String
  @@inherit.extends("Base")
  @@map("wr_py_child")
}
"""


@pytest.fixture(params=["sqlite", "postgresql"])
async def rules_world(request):
    import os

    postgres = request.param == "postgresql"
    url = os.environ.get("ORM_TEST_DATABASE_URL") if postgres else "sqlite://:memory:"
    if url is None:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    roles = '\n  bypass_roles = ["postgres"]' if postgres else ""
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n  provider = "{request.param}"{roles}\n}}\n{RULES}', registry=registry)
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield postgres, db, models
    finally:
        await db.drop_tables()
        await db.close()


async def test_write_rule_layers_agree_on_which_writes_they_refuse(rules_world):
    _, db, models = rules_world
    ledger, child, frozen = (models[name] for name in ("Ledger", "Child", "Frozen"))
    rows = ledger.objects.using(db)
    await rows.insert_many([{"id": 1, "note": "open", "slug": "a"}, {"id": 2, "note": "locked", "slug": "b"}])
    row = rows.filter(ledger.id == 1)
    await row.update(note="edited")
    with pytest.raises(monorm.WriteProtected, match="Ledger"):
        await row.update(slug="a")
    assert await db.execute("UPDATE wr_py_ledger SET slug = 'a' WHERE id = 1") == 1
    with pytest.raises(monorm.WriteProtected, match="read_only_slug"):
        await db.execute("UPDATE wr_py_ledger SET slug = 'new' WHERE id = 1")
    with pytest.raises(monorm.WriteProtected):
        await rows.filter(ledger.id == 2).update(note="editable")
    with pytest.raises(monorm.WriteProtected):
        await row.delete()
    with monorm.allow_writes(ledger), pytest.raises(monorm.WriteProtected):
        await row.delete()
    with monorm.allow_writes(frozen), pytest.raises(monorm.WriteProtected):
        await frozen.objects.using(db).insert(id=1)
    assert await db.execute("INSERT INTO wr_py_dropped_table_with_a_long_name_for_introspection (id) VALUES (1)") == 0
    await child.objects.using(db).insert(id=1, note="a")
    with pytest.raises(monorm.WriteProtected, match="Child"):
        await child.objects.using(db).filter(child.id == 1).update(note="b")


async def test_a_child_of_a_protected_base_refuses_a_raw_update_in_its_own_table(rules_world):
    _, db, models = rules_world
    child = models["Child"]
    await child.objects.using(db).insert_many([{"id": 1, "note": "a"}, {"id": 2, "note": "b"}])
    with pytest.raises(monorm.WriteProtected, match="protect_"):
        await db.execute("UPDATE wr_py_child SET note = 'raw' WHERE id = 1")
    assert (await child.objects.using(db).get(child.id == 1)).note == "a"
    # The inherited rule names the update only, so the database still takes a raw delete.
    assert await db.execute("DELETE FROM wr_py_child WHERE id = 2") == 1


async def test_allow_writes_opens_only_skippable_rules_for_approved_roles(rules_world):
    postgres, db, models = rules_world
    model = models["Openable"]
    rows = model.objects.using(db)
    with pytest.raises(monorm.WriteProtected, match="Openable"):
        await rows.insert(id=1)
    with monorm.allow_writes(model):
        await rows.insert(id=1)
        async with db.transaction():
            await rows.insert(id=2).prepare()()
    if not postgres:
        return
    with pytest.raises(monorm.WriteProtected):
        await db.execute("INSERT INTO wr_py_openable (id) VALUES (3)")
    with monorm.allow_writes(model):
        assert await db.execute("INSERT INTO wr_py_openable (id) VALUES (3)") == 1
    with pytest.raises(monorm.WriteProtected, match="monorm write rule"), monorm.allow_writes(model):
        async with db.transaction():
            await db.execute("CREATE ROLE monorm_wr_py_scoped_plain")
            await db.execute("GRANT INSERT, SELECT ON wr_py_openable TO monorm_wr_py_scoped_plain")
            await db.execute("SET LOCAL SESSION AUTHORIZATION monorm_wr_py_scoped_plain")
            await rows.insert(id=4)
    assert await rows.count() == 3


async def test_pull_recovers_rule_attributes(rules_world):
    from monorm.migrations import pull

    _, db, _ = rules_world
    pulled = await pull(db)
    assert "@write_rules.read_only" in pulled.schema
    assert "@@write_rules.protect(operation: [update], layer: db, condition:" in pulled.schema
    assert "@@write_rules.protect(operation: [insert], layer: db, action: drop)" in pulled.schema
    assert not [gap for gap in pulled.gaps if "wr_py_" in gap or "write rule claims" in gap]
