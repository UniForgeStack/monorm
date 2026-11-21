"""Linked relations (`path: [...]`) on PostgreSQL and SQLite: filter, `alias()` independence, `load()`, order by, `count()`, writes."""

from pathlib import Path

import pytest

import monorm
from artifact_caps import has_extension
from conftest import DATABASE_URL
from monorm import exists, outer
from monorm.migrations import Migrations

SCHEMA = """
model Country {
  id   BigInt @id @default(autoincrement())
  name String
  @@map("lr_country")
}

model City {
  id         BigInt  @id @default(autoincrement())
  name       String
  country_id BigInt
  country    Country @relation(fields: [country_id], references: [id])
  @@map("lr_city")
}

model Address {
  id      BigInt @id @default(autoincrement())
  city_id BigInt
  city    City   @relation(fields: [city_id], references: [id])
  @@map("lr_address")
}

model User {
  id         BigInt   @id @default(autoincrement())
  name       String
  address_id BigInt?
  address    Address? @relation(fields: [address_id], references: [id])
  posts      Post[]
  comments   Comment[]
  commenters User[]   @relation(path: [posts, comments, author])
  country    Country? @relation(path: [address, city, country])
  @@map("lr_user")
}

model Post {
  id        BigInt    @id @default(autoincrement())
  author_id BigInt
  author    User      @relation(fields: [author_id], references: [id])
  published Boolean
  comments  Comment[]
  @@map("lr_post")
}

model Comment {
  id        BigInt @id @default(autoincrement())
  post_id   BigInt
  post      Post   @relation(fields: [post_id], references: [id])
  author_id BigInt
  author    User   @relation(fields: [author_id], references: [id])
  @@map("lr_comment")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def data(request):
    registry = monorm.Registry()
    source = SCHEMA if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + SCHEMA
    m = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Country, City, Address, User, Post, Comment = (m[n] for n in ("Country", "City", "Address", "User", "Post", "Comment"))
    fr, de = await Country.objects.using(db).insert_many([{"name": "France"}, {"name": "Germany"}]).returning()
    paris, berlin = await City.objects.using(db).insert_many(
        [{"name": "Paris", "country_id": fr.id}, {"name": "Berlin", "country_id": de.id}]
    ).returning()
    a_paris, a_berlin = await Address.objects.using(db).insert_many([{"city_id": paris.id}, {"city_id": berlin.id}]).returning()
    ann, bob, cy, dee = await User.objects.using(db).insert_many(
        [
            {"name": "ann", "address_id": a_berlin.id},
            {"name": "bob", "address_id": a_paris.id},
            {"name": "cy", "address_id": None},
            {"name": "dee", "address_id": a_paris.id},
        ]
    ).returning()
    # ann has two posts, bob one; cy comments twice on ann's two posts (one target through two middle rows), dee on one
    p1, p2, p3 = await Post.objects.using(db).insert_many(
        [
            {"author_id": ann.id, "published": True},
            {"author_id": ann.id, "published": False},
            {"author_id": bob.id, "published": True},
        ]
    ).returning()
    await Comment.objects.using(db).insert_many(
        [
            {"post_id": p1.id, "author_id": cy.id},
            {"post_id": p2.id, "author_id": cy.id},
            {"post_id": p2.id, "author_id": dee.id},
            {"post_id": p3.id, "author_id": ann.id},
        ]
    ).returning()
    yield db, User, Post, Comment, Country
    await db.drop_tables()
    await db.close()


async def names(db, qs):
    return sorted(u.name for u in await qs.using(db))


async def test_filter_across_the_chain(data):
    db, User, *_ = data
    assert await names(db, User.objects.filter(User.commenters.name == "cy")) == ["ann"]
    assert await names(db, User.objects.filter(User.commenters.name == "ann")) == ["bob"]
    assert await names(db, User.objects.filter(User.commenters.name == "nobody")) == []


async def test_a_linked_relation_is_its_own_path(data):
    db, User, *_ = data
    # `commenters` does not share its middle rows with `posts`: ann's unpublished post has dee, her published post has not
    assert await names(db, User.objects.filter(User.commenters.name == "dee", User.posts.published == True)) == ["ann"]  # noqa: E712
    assert await names(db, User.objects.filter(User.posts.comments.author.name == "dee", User.posts.published == True)) == []  # noqa: E712


async def test_load_gives_each_target_once(data):
    db, User, *_ = data
    users = {u.name: u for u in await User.objects.using(db).load(User.commenters)}
    assert sorted(c.name for c in users["ann"].commenters) == ["cy", "dee"]
    assert [c.name for c in users["bob"].commenters] == ["ann"]
    assert users["cy"].commenters == [] and users["dee"].commenters == []


async def test_a_to_one_chain_loads_with_joins_and_orders(data):
    db, User, *_ = data
    users = {u.name: u for u in await User.objects.using(db).load(User.country)}
    assert {n: (u.country.name if u.country else None) for n, u in users.items()} == {"ann": "Germany", "bob": "France", "cy": None, "dee": "France"}
    ordered = [u.name for u in await User.objects.using(db).filter(User.address_id != None).order_by(User.country.name, User.name)]  # noqa: E711
    assert ordered == ["bob", "dee", "ann"]
    assert await names(db, User.objects.filter(User.country.name == "France")) == ["bob", "dee"]


async def test_outer_reads_through_a_linked_to_one(data):
    db, User, *_, Country = data
    # the subquery correlates to each user through the flat hops: cy has no address, so no country, so no row
    inner = Country.objects.filter(Country.name == outer(User.country.name))
    assert await names(db, User.objects.filter(exists(inner))) == ["ann", "bob", "dee"]


async def test_or_null_on_a_linked_to_one_adds_the_missing_row(data):
    db, User, *_ = data
    # a linked to-one is a to-one path: the not-null column adds NOT EXISTS, it does not raise
    assert await names(db, User.objects.filter((User.country.name == "France").or_null())) == ["bob", "cy", "dee"]


async def test_order_by_a_to_many_chain_is_refused(data):
    db, User, *_ = data
    with pytest.raises(Exception, match="to-many"):
        await User.objects.using(db).order_by(User.commenters.name)


async def test_count_counts_distinct_targets(data):
    db, User, *_ = data
    assert await User.objects.using(db).filter(User.commenters.name == "cy").count() == 1
    assert await User.objects.using(db).filter(User.commenters.id > 0).count() == 2  # ann (cy twice) and bob: users, not paths


async def test_not_loaded_and_writes_raise(data):
    db, User, *_ = data
    ann = await User.objects.using(db).get(User.name == "ann")
    with pytest.raises(monorm.NotLoaded):
        ann.commenters
    with pytest.raises(monorm.NotLoaded):
        ann.country
    with pytest.raises(AttributeError, match="read-only"):
        ann.commenters = []
    with pytest.raises(AttributeError, match="read-only"):
        ann.country = None
    # no manager: a loaded linked relation is a plain list, so add, remove and set do not exist
    ann = await User.objects.using(db).load(User.commenters).get(User.name == "ann")
    assert type(ann.commenters) is list
    for call in ("add", "set"):
        assert not hasattr(ann.commenters, call)
    # the class attribute is a path, not a manager: a write through it has no method either
    for call in ("add", "remove", "set"):
        with pytest.raises(AttributeError):
            getattr(User.commenters, call)


async def test_a_write_through_a_linked_relation_raises(data):
    db, User, *_ = data
    for name, call in (
        ("commenters", lambda: User.objects.using(db).insert(name="zed", commenters=[])),
        ("country", lambda: User.objects.using(db).insert(name="zed", country=None)),
        ("commenters", lambda: User.objects.using(db).filter(User.name == "ann").update(commenters=[])),
    ):
        with pytest.raises(TypeError, match=rf"User\.{name} is a linked relation: read-only"):
            await call()


CHAIN = """
model LhGroup {
  id   Int     @id @default(autoincrement())
  name String
  tags LhTag[]
  @@map("lh_group")
}

model LhTag {
  id       Int        @id @default(autoincrement())
  name     String
  group_id Int
  group    LhGroup    @relation(fields: [group_id], references: [id])
  books    LhBook[]   @relation(through: LhBookTag)
  links    LhBookTag[]
  @@map("lh_tag")
}

model LhAuthor {
  id     Int      @id @default(autoincrement())
  name   String
  books  LhBook[]
  tags   LhTag[]   @relation(path: [books, tags])
  groups LhGroup[] @relation(path: [tags, group])
  @@map("lh_author")
}

model LhBook {
  id        Int        @id @default(autoincrement())
  author_id Int
  author    LhAuthor   @relation(fields: [author_id], references: [id])
  tags      LhTag[]    @relation(through: LhBookTag)
  links     LhBookTag[]
  @@map("lh_book")
}

model LhBookTag {
  id      Int    @id @default(autoincrement())
  book_id Int
  tag_id  Int
  book    LhBook @relation(fields: [book_id], references: [id])
  tag     LhTag  @relation(fields: [tag_id], references: [id])
  @@map("lh_book_tag")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def chain(request):
    registry = monorm.Registry()
    source = CHAIN if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + CHAIN
    m = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Group, Tag, Author, Book, BookTag = (m[n] for n in ("LhGroup", "LhTag", "LhAuthor", "LhBook", "LhBookTag"))
    warm, cool = await Group.objects.using(db).insert_many([{"name": "warm"}, {"name": "cool"}]).returning()
    red, blue, green = await Tag.objects.using(db).insert_many(
        [{"name": "red", "group_id": warm.id}, {"name": "blue", "group_id": cool.id}, {"name": "green", "group_id": cool.id}]
    ).returning()
    ann, bob, _cy = await Author.objects.using(db).insert_many([{"name": "ann"}, {"name": "bob"}, {"name": "cy"}]).returning()
    b1, b2, b3 = await Book.objects.using(db).insert_many(
        [{"author_id": ann.id}, {"author_id": ann.id}, {"author_id": bob.id}]
    ).returning()
    # ann reaches blue through two books: one target through two middle rows
    await BookTag.objects.using(db).insert_many(
        [
            {"book_id": b1.id, "tag_id": red.id},
            {"book_id": b1.id, "tag_id": blue.id},
            {"book_id": b2.id, "tag_id": blue.id},
            {"book_id": b3.id, "tag_id": green.id},
        ]
    )
    yield db, Author
    await db.drop_tables()
    await db.close()


async def test_a_many_to_many_hop_in_a_chain_filters_loads_and_counts(chain):
    db, Author = chain
    assert await names(db, Author.objects.filter(Author.tags.name == "blue")) == ["ann"]
    assert await names(db, Author.objects.filter(Author.tags.name == "green")) == ["bob"]
    authors = {a.name: a for a in await Author.objects.using(db).load(Author.tags)}
    assert sorted(t.name for t in authors["ann"].tags) == ["blue", "red"]
    assert [t.name for t in authors["bob"].tags] == ["green"] and authors["cy"].tags == []
    assert await Author.objects.using(db).filter(Author.tags.id > 0).count() == 2


async def test_a_linked_hop_in_a_chain_filters_and_loads(chain):
    db, Author = chain
    assert await names(db, Author.objects.filter(Author.groups.name == "cool")) == ["ann", "bob"]
    assert await names(db, Author.objects.filter(Author.groups.name == "warm")) == ["ann"]
    authors = {a.name: a for a in await Author.objects.using(db).load(Author.groups)}
    assert sorted(g.name for g in authors["ann"].groups) == ["cool", "warm"]
    assert [g.name for g in authors["bob"].groups] == ["cool"] and authors["cy"].groups == []


TREE = """
model Node {
  id            BigInt @id @default(autoincrement())
  name          String
  parent_id     BigInt?
  parent        Node?  @relation("Tree", fields: [parent_id], references: [id])
  children      Node[] @relation("Tree")
  grandchildren Node[] @relation(path: [children, children])
  grandparent   Node?  @relation(path: [parent, parent])
  @@map("lr_node")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def tree(request):
    registry = monorm.Registry()
    source = TREE if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + TREE
    m = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Node = m["Node"]
    root = await Node.objects.using(db).insert(name="root")
    a, b = await Node.objects.using(db).insert_many([{"name": "a", "parent_id": root.id}, {"name": "b", "parent_id": root.id}]).returning()
    await Node.objects.using(db).insert_many(
        [{"name": "a1", "parent_id": a.id}, {"name": "a2", "parent_id": a.id}, {"name": "b1", "parent_id": b.id}]
    ).returning()
    yield db, Node
    await db.drop_tables()
    await db.close()


async def test_a_self_tree_loads_and_filters(tree):
    db, Node = tree
    nodes = {n.name: n for n in await Node.objects.using(db).load(Node.parent, Node.children)}
    assert nodes["root"].parent is None
    assert nodes["a1"].parent.name == "a"
    assert sorted(c.name for c in nodes["a"].children.cached) == ["a1", "a2"]
    assert nodes["a1"].children.cached == []
    assert await names(db, Node.objects.filter(Node.parent.name == "a")) == ["a1", "a2"]
    assert await names(db, Node.objects.filter(Node.children.name == "a1")) == ["a"]
    assert await names(db, Node.objects.filter(Node.parent.parent.name == "root")) == ["a1", "a2", "b1"]


async def test_a_linked_relation_repeats_one_relation_of_a_self_tree(tree):
    db, Node = tree
    assert await names(db, Node.objects.filter(Node.grandchildren.name == "a2")) == ["root"]
    assert await names(db, Node.objects.filter(Node.grandparent.name == "root")) == ["a1", "a2", "b1"]
    nodes = {n.name: n for n in await Node.objects.using(db).load(Node.grandchildren, Node.grandparent)}
    assert sorted(n.name for n in nodes["root"].grandchildren) == ["a1", "a2", "b1"]
    assert nodes["a"].grandchildren == []
    assert nodes["a1"].grandparent.name == "root" and nodes["a"].grandparent is None


def _constraint_schema(directory: Path, dialect: str, *, constraint: str = "constraint: false") -> Path:
    head = 'datasource db { provider = "sqlite" }\n' if dialect == "sqlite" else ""
    extra = f", {constraint}" if constraint else ""
    path = directory / "schema.prisma"
    path.write_text(head + f"""
model LcItem {{
  id   Int    @id
  name String
  @@map("lc_item")
}}
view LcItemView {{
  id    Int      @id
  name  String
  stats LcStat[]
  @@map("lc_item_view")
}}
model LcStat {{
  id      Int        @id
  item_id Int
  item    LcItemView @relation(fields: [item_id], references: [id]{extra})
  twin_id Int
  twin    LcItem     @relation("Twin", fields: [twin_id], references: [id]{extra})
  @@map("lc_stat")
}}
""")
    sidecars = directory / "views" / ("main" if dialect == "sqlite" else "public")
    sidecars.mkdir(parents=True, exist_ok=True)
    (sidecars / "LcItemView.sql").write_text('SELECT id, name FROM "lc_item"')
    return path


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_constraint_false_reads_and_writes_without_a_foreign_key(dialect, tmp_path):
    schema = _constraint_schema(tmp_path, dialect)
    registry = monorm.Registry()
    m = monorm.load(schema, registry=registry)
    sql = Migrations(tmp_path / "migrations", schema).make()[0].up_sql
    assert "lc_stat" in sql and "FOREIGN KEY" not in sql and "REFERENCES" not in sql
    url = "sqlite://:memory:" if dialect == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        Item, View, Stat = m["LcItem"], m["LcItemView"], m["LcStat"]
        await Item.objects.using(db).insert_many([{"id": 1, "name": "one"}, {"id": 2, "name": "two"}])
        # a row for a key that no row holds: no constraint refuses it
        await Stat.objects.using(db).insert_many([{"id": 1, "item_id": 1, "twin_id": 1}, {"id": 2, "item_id": 2, "twin_id": 99}])
        stats = {s.id: s for s in await Stat.objects.using(db).load(Stat.item, Stat.twin)}
        assert stats[1].item.name == "one" and stats[2].item.name == "two"
        assert stats[1].twin.name == "one"
        # as for any relation, a key that no row holds is not a loaded row
        with pytest.raises(monorm.NotLoaded, match="no row of the target has it"):
            stats[2].twin
        assert sorted(s.id for s in await Stat.objects.using(db).filter(Stat.item.name == "two")) == [2]
        view = await View.objects.using(db).load(View.stats).get(View.id == 1)
        assert [s.id for s in view.stats.cached] == [1]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
def test_the_migration_diff_follows_the_constraint_flag(dialect, tmp_path):
    schema = _constraint_schema(tmp_path, dialect, constraint="")
    migrations = Migrations(tmp_path / "migrations", schema)
    first = migrations.make()[0].up_sql
    # a foreign key to a table, none to a view that cannot hold one
    assert "REFERENCES" in first
    _constraint_schema(tmp_path, dialect, constraint="constraint: false")
    (drop,) = migrations.make("drop_fk")
    assert ("DROP CONSTRAINT" in drop.up_sql) if dialect == "postgres" else ("REFERENCES" not in drop.up_sql)
    _constraint_schema(tmp_path, dialect, constraint="")
    (add,) = migrations.make("add_fk")
    assert "REFERENCES" in add.up_sql


@pytest.mark.parametrize("bad", ["onDelete: Cascade", "onUpdate: Cascade", "deferrable: true"])
def test_constraint_false_refuses_a_database_action(bad):
    source = SCHEMA.replace("author    User      @relation(fields: [author_id], references: [id])", f"author    User      @relation(fields: [author_id], references: [id], constraint: false, {bad})", 1)
    with pytest.raises(Exception, match="constraint"):
        monorm.loads(source, registry=monorm.Registry())


GUARD = """
model GReader {
  id   BigInt @id
  name String
  @@map("lr_g_reader")
}

model GOrg {
  id      BigInt  @id
  name    String
  posts   GPost[]
  readers GReader[] @relation(path: [posts, reads, reader])
  @@map("lr_g_org")
}

model GPost {
  id     BigInt  @id
  org_id BigInt
  org    GOrg    @relation(fields: [org_id], references: [id])
  shop   Int
  reads  GRead[]
  @@soft_delete.deleted_at("deleted")
  @@query.filter("tenant", shop == scope.shop)
  @@map("lr_g_post")
}

model GRead {
  id        BigInt  @id
  post_id   BigInt
  post      GPost   @relation(fields: [post_id], references: [id])
  reader_id BigInt
  reader    GReader @relation(fields: [reader_id], references: [id])
  @@map("lr_g_read")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def guard(request):
    if not has_extension("monorm.soft-delete"):
        pytest.skip("requires the soft-delete package")
    registry = monorm.Registry()
    source = GUARD if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + GUARD
    m = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Org, Post, Read, Reader = m["GOrg"], m["GPost"], m["GRead"], m["GReader"]
    await Reader.objects.using(db).insert_many([{"id": 1, "name": "r1"}, {"id": 2, "name": "r2"}])
    await Org.objects.using(db).insert(id=1, name="org")
    with monorm.scope(shop=1):
        await Post.objects.using(db).insert_many([{"id": 1, "org_id": 1, "shop": 1}, {"id": 2, "org_id": 1, "shop": 1}])
    with monorm.scope(shop=2):
        await Post.objects.using(db).insert(id=3, org_id=1, shop=2)
    await Read.objects.using(db).insert_many(
        [{"id": 1, "post_id": 1, "reader_id": 1}, {"id": 2, "post_id": 2, "reader_id": 2}, {"id": 3, "post_id": 3, "reader_id": 1}]
    )
    yield db, Org, Post
    await db.drop_tables()
    await db.close()


async def test_a_soft_deleted_middle_row_hides_the_target(guard):
    db, Org, Post = guard
    with monorm.scope(shop=1):
        org = await Org.objects.using(db).load(Org.readers).get(Org.id == 1)
        assert sorted(r.name for r in org.readers) == ["r1", "r2"]
        assert await Org.objects.using(db).filter(Org.readers.name == "r2").count() == 1
        await Post.objects.using(db).filter(Post.id == 2).delete()
        org = await Org.objects.using(db).load(Org.readers).get(Org.id == 1)
        assert [r.name for r in org.readers] == ["r1"]
        assert await Org.objects.using(db).filter(Org.readers.name == "r2").count() == 0


async def test_the_scope_policy_of_every_table_on_the_path_applies(guard):
    db, Org, Post = guard
    # post 3 (shop 2) is the only path to nothing but r1; shop 1 sees posts 1 and 2
    with monorm.scope(shop=2):
        org = await Org.objects.using(db).load(Org.readers).get(Org.id == 1)
        assert [r.name for r in org.readers] == ["r1"]
        assert await Org.objects.using(db).filter(Org.readers.name == "r2").count() == 0
    with monorm.scope(shop=1):
        assert await Org.objects.using(db).filter(Org.readers.name == "r2").count() == 1
