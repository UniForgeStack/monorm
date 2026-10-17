"""Query-set API: custom query-set classes, prefetch on loaded instances, columns through
to-one `load()` paths, OR of query sets, single-row labels, column paths and model metadata."""

import os
import sys
from decimal import Decimal

import pytest
from blog.models import Comment, Post, PostTag, Profile, Tag, User

import orm
from orm import QuerySet, use_query_set


@pytest.fixture
def custom():
    use_query_set(Post, "_blog_queries:PostQueries")
    use_query_set(Tag, "_blog_queries:TagQueries")
    yield sys.modules.get("_blog_queries")
    use_query_set(Post, QuerySet)
    use_query_set(Tag, QuerySet)


async def seed():
    alice = await User.objects.insert(email="alice@example.com", name="Alice")
    bob = await User.objects.insert(email="bob@example.com", name="Bob")
    posts = await Post.objects.insert_many(
        [
            {"author": alice, "title": "draft", "body": "...", "views": 50},
            {"author": alice, "title": "hit", "body": "...", "published": True, "views": 90},
            {"author": alice, "title": "quiet", "body": "...", "published": True, "views": 1},
            {"author": bob, "title": "bob", "body": "...", "published": True, "views": 70},
        ]
    ).returning()
    return alice, bob, posts


# -- custom query-set classes (F4) ---------------------------------------------------------


async def test_custom_methods_chain_with_builder_methods(clean, custom):
    alice, _, _ = await seed()
    qs = Post.objects.published().filter(Post.author_id == alice.id).popular(10)
    assert type(qs).__name__ == "PostQueries"
    assert [p.title for p in await qs] == ["hit"]
    assert [p.title for p in await Post.objects.order_by(Post.views).popular(60).published()] == ["bob", "hit"]
    assert await Post.objects.published().count() == 3


async def test_class_path_is_imported_on_first_use(clean):
    sys.modules.pop("_blog_queries", None)
    use_query_set(Post, "_blog_queries:PostQueries")
    try:
        assert "_blog_queries" not in sys.modules
        assert type(Post.objects).__name__ == "PostQueries"
        assert "_blog_queries" in sys.modules
        assert Post.objects is Post.objects  # the root query set replaces the lazy one
    finally:
        use_query_set(Post, QuerySet)
    assert type(Post.objects) is QuerySet


async def test_relation_sets_have_the_custom_methods(clean, custom):
    alice, _, posts = await seed()
    assert [p.title for p in await alice.posts.published().order_by(Post.views)] == ["quiet", "hit"]
    assert [p.title for p in await alice.posts.filter(Post.views > 5).published()] == ["hit"]
    assert isinstance(alice.posts, orm.RelatedSet)
    tags = await Tag.objects.insert_many([{"name": "python"}, {"name": "rust"}]).returning()
    await posts[1].tags.add(*tags)
    assert [t.name for t in await posts[1].tags.named("py")] == ["python"]
    assert isinstance(posts[1].tags, orm.ManyRelatedSet)


async def test_prefetch_query_set_uses_custom_methods(clean, custom):
    await seed()
    users = await User.objects.order_by(User.name).load(User.posts.objects.published())
    assert [[p.title for p in u.posts.cached] for u in users] == [["hit", "quiet"], ["bob"]]


def test_query_set_class_is_checked():
    with pytest.raises(TypeError, match="subclass of orm.QuerySet"):
        use_query_set(Post, dict)  # type: ignore[arg-type]

    class Slotted(QuerySet[Post]):
        __slots__ = ("x",)

    with pytest.raises(TypeError, match="__slots__"):
        use_query_set(Post, Slotted)
    use_query_set(Post, "_blog_queries")
    try:
        with pytest.raises(TypeError, match="module:Class"):
            Post.objects
    finally:
        use_query_set(Post, QuerySet)


CHECK_QUERY_SETS = """\
from typing_extensions import assert_type

from app.models import Post, User, UserQuerySet
from app.queries import PostQueries

assert_type(Post.objects, PostQueries)
assert_type(Post.objects.published().filter(Post.views > 1).order_by(Post.id).published(), PostQueries)
assert_type(User.objects.filter(User.id == 1), UserQuerySet)
Post.objects.insert(title="t", body="b", author_id=1)
Post.objects.nope()  # E
Post.objects.published().insert(title=1)  # E
User.objects.published()  # E
"""


def _query_set_project(tmp_path):
    from pathlib import Path

    app = tmp_path / "app"
    app.mkdir()
    (app / "__init__.py").touch()
    (app / "schema.prisma").write_text((Path(__file__).parents[3] / "examples/blog/schema.prisma").read_text())
    (app / "queries.py").write_text(
        "from typing_extensions import Self\n\nfrom app.models import Post, PostQuerySet\n\n\n"
        "class PostQueries(PostQuerySet):\n    def published(self) -> Self:\n        return self.filter(Post.published)\n"
    )
    code = orm._native.cli(
        [
            "--schema",
            str(app / "schema.prisma"),
            "generate",
            "python",
            "-o",
            str(app / "models.py"),
            "--query-set",
            "Post=app.queries:PostQueries",
        ]
    )
    assert code == 0
    (tmp_path / "check.py").write_text(CHECK_QUERY_SETS)
    return {i for i, line in enumerate(CHECK_QUERY_SETS.splitlines(), 1) if "# E" in line}


def test_generated_query_set_types(tmp_path):
    import re
    import shutil
    import subprocess
    from pathlib import Path

    pytest.importorskip("mypy")
    expected = _query_set_project(tmp_path)
    root = Path(__file__).parent.parent
    out = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", "check.py"],
        capture_output=True,
        text=True,
        env={"MYPYPATH": f"{root}:{tmp_path}", "PATH": ""},
        cwd=tmp_path,
    ).stdout
    assert {int(n) for n in re.findall(r"^check\.py:(\d+): error", out, re.M)} == expected, out
    exe = shutil.which("pyright")
    if exe is not None:
        (tmp_path / "pyrightconfig.json").write_text(
            f'{{"extraPaths": ["{root}"], "typeCheckingMode": "strict", "reportUnusedExpression": false}}'
        )
        out = subprocess.run([exe, "check.py"], capture_output=True, text=True, cwd=tmp_path).stdout
        assert {int(n) for n in re.findall(r"check\.py:(\d+):\d+ - error", out)} == expected, out
    run = subprocess.run(
        [sys.executable, "-c", "from app.models import Post; print(type(Post.objects).__name__)"],
        capture_output=True,
        text=True,
        cwd=tmp_path,
        env={**os.environ, "PYTHONPATH": os.pathsep.join(filter(None, [str(tmp_path), os.environ.get("PYTHONPATH")]))},
    )
    assert run.stdout.strip() == "PostQueries", run.stderr


def test_cli_reads_query_sets_from_pyproject(tmp_path, monkeypatch, capfd):
    (tmp_path / "schema.prisma").write_text("model Book {\n id Int @id\n}\n")
    (tmp_path / "pyproject.toml").write_text('[tool.orm.query_sets]\nBook = "app.queries:BookQueries"\n')
    monkeypatch.chdir(tmp_path)
    assert orm._native.cli(["generate", "python", "-o", "models.py"]) == 0
    assert 'use_query_set(Book, "app.queries:BookQueries")' in (tmp_path / "models.py").read_text()
    assert "objects: ClassVar[_BookObjects]" in (tmp_path / "models.pyi").read_text()
    assert orm._native.cli(["generate", "python", "-o", "models.py", "--query-set", "Book=nomodule"]) == 1
    assert "expected module:Class" in capfd.readouterr().err


def test_cli_rejects_duplicate_query_set_flags(tmp_path, monkeypatch, capfd):
    (tmp_path / "schema.prisma").write_text("model Book {\n id Int @id\n}\n")
    monkeypatch.chdir(tmp_path)
    assert (
        orm._native.cli(["generate", "python", "-o", "models.py", "--query-set", "Book=a:B", "--query-set", "Book=c:D"])
        == 2
    )
    assert "duplicate --query-set for model Book" in capfd.readouterr().err
    assert not (tmp_path / "models.py").exists()


def test_cli_query_set_flag_overrides_config(tmp_path, monkeypatch):
    (tmp_path / "schema.prisma").write_text("model Book {\n id Int @id\n}\n")
    (tmp_path / "pyproject.toml").write_text('[tool.orm.query_sets]\nBook = "app.queries:BookQueries"\n')
    monkeypatch.chdir(tmp_path)
    assert orm._native.cli(["generate", "python", "-o", "models.py", "--query-set", "Book=other:Queries"]) == 0
    assert 'use_query_set(Book, "other:Queries")' in (tmp_path / "models.py").read_text()
    assert "from other import Queries as _BookObjects" in (tmp_path / "models.pyi").read_text()


# -- prefetch on loaded instances (F10) -----------------------------------------------------


async def test_prefetch_onto_loaded_instances(clean):
    alice, bob, posts = await seed()
    await Comment.objects.insert_many([{"post": posts[1], "body": "a"}, {"post": posts[1], "body": "b"}])
    users = await User.objects.order_by(User.id)
    top = User.posts.objects.filter(Post.published)[:1].label("top")
    result = User.objects.load(User.posts.comments, top).inspect()
    assert result.statement_count == 4  # root plus three prefetch templates
    assert result.has_batching
    assert [s.has_batching for s in result.statements] == [False, True, True, True]
    assert all(s.joins_count == 0 for s in result.statements)
    assert sum(s.uncorrelated_count for s in result.statements) == 1
    await orm.prefetch(users, User.posts.comments, top)
    assert [p.title for p in users[0].posts.cached] == ["draft", "hit", "quiet"]
    assert [c.body for c in users[0].posts.cached[1].comments.cached] == ["a", "b"]
    assert users[0].posts.cached[0].author is users[0]
    assert [[p.title for p in u.top] for u in users] == [["hit"], ["bob"]]


async def test_prefetch_to_one_and_many_to_many(clean):
    _, _, posts = await seed()
    tags = await Tag.objects.insert_many([{"name": "a"}, {"name": "b"}]).returning()
    await posts[0].tags.add(*tags)
    loaded = await Post.objects.order_by(Post.id)
    # The equivalent load describes the batched many-to-many statement too.
    assert Post.objects.load(Post.author, Post.tags).inspect() == orm.QueryInspection(
        2,
        True,
        (
            orm.StatementInspection(False, 1, 0, 0, 0),
            orm.StatementInspection(True, 1, 0, 0, 0),
        ),
    )
    await orm.prefetch(loaded, Post.author, Post.tags)
    assert [p.author.name for p in loaded] == ["Alice", "Alice", "Alice", "Bob"]
    assert [t.name for t in loaded[0].tags.cached] == ["a", "b"] and loaded[1].tags.cached == []


async def test_prefetch_reads_keys_from_the_instances(clean):
    _, _, posts = await seed()
    await Comment.objects.insert_many([{"post": posts[0], "body": "a"}, {"post": posts[3], "body": "b"}])
    comments = await Comment.objects.order_by(Comment.id)
    await Comment.objects.delete()  # the instances' own rows are not read again
    await orm.prefetch(comments, Comment.post.author)
    assert [(c.post.title, c.post.author.name) for c in comments] == [("draft", "Alice"), ("bob", "Bob")]


async def test_prefetch_checks_its_input(clean):
    alice, _, posts = await seed()
    await orm.prefetch([], User.posts)
    with pytest.raises(TypeError, match="one model"):
        await orm.prefetch([alice, posts[0]], User.posts)
    with pytest.raises(ValueError, match="does not start at User"):
        await orm.prefetch([alice], Post.author)


DECIMAL_KEYS = """datasource db { provider = "postgres" }
model DecParent {
  id Decimal @id @db.Decimal(10, 2)
  kids DecKid[]
  @@map("g6_dec_parents")
}
model DecKid {
  id Int @id @default(autoincrement())
  dec_id Decimal @db.Decimal(12, 4)
  dec DecParent @relation(fields: [dec_id], references: [id])
  @@map("g6_dec_kids")
}
"""


@pytest.fixture
async def decimal_keys():
    from conftest import DATABASE_URL

    registry = orm.Registry()
    models = orm.loads(DECIMAL_KEYS, registry=registry)
    db = await orm.connect(DATABASE_URL, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        parent, kid = models["DecParent"], models["DecKid"]
        await parent.objects.using(db).insert_many([{"id": Decimal("1.50")}, {"id": Decimal("2.00")}])
        await kid.objects.using(db).insert_many([{"dec_id": Decimal("1.50")}, {"dec_id": Decimal("1.50")}])
        yield parent, kid, db
    finally:
        await db.drop_tables()
        await db.close()


async def test_prefetch_decimal_key_to_many(decimal_keys):
    parent, _, db = decimal_keys
    decs = await parent.objects.using(db).order_by(parent.id)
    await orm.prefetch(decs, parent.kids, using=db)
    assert [len(d.kids.cached) for d in decs] == [2, 0]  # the child column has scale 4


async def test_prefetch_decimal_key_to_one(decimal_keys):
    _, kid, db = decimal_keys
    kids = await kid.objects.using(db).order_by(kid.id)
    await orm.prefetch(kids, kid.dec, using=db)
    assert [k.dec.id for k in kids] == [Decimal("1.50")] * 2


@pytest.fixture
async def prefetch_databases():
    registry = orm.Registry()
    models = orm.loads(
        """datasource db { provider = "sqlite" }
model Author {
  id Int @id @default(autoincrement())
  name String
  books Book[]
}
model Book {
  id Int @id @default(autoincrement())
  title String
  author_id Int
  author Author @relation(fields: [author_id], references: [id])
}
""",
        registry=registry,
    )
    Author, Book = models["Author"], models["Book"]
    d1 = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    d2 = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        for db, name in ((d1, "d1"), (d2, "d2")):
            await db.create_tables()
            authors = await Author.objects.using(db).insert_many([{"name": name}, {"name": "empty"}]).returning()
            await Book.objects.using(db).insert(title=f"B-{name}", author_id=authors[0].id)
        a1 = await Author.objects.using(d1).order_by(Author.id).first()
        x, y = await Author.objects.using(d2).order_by(Author.id)
        yield Author, d2, [a1, y, x]
    finally:
        await d1.close()
        await d2.close()


async def test_prefetch_refuses_instances_from_two_databases(prefetch_databases):
    Author, _, objs = prefetch_databases
    with pytest.raises(
        orm.QueryError, match=r"prefetch\(\) takes instances from one database; pass using= to read another"
    ):
        await orm.prefetch(objs, Author.books)
    # Rejection must leave every relation unloaded.
    for obj in objs:
        with pytest.raises(orm.NotLoaded):
            obj.books.cached


async def test_prefetch_with_using_reads_that_database(prefetch_databases):
    Author, d2, objs = prefetch_databases
    await orm.prefetch(objs, Author.books, using=d2)
    assert [[b.title for b in obj.books.cached] for obj in objs] == [["B-d2"], [], ["B-d2"]]


async def test_prefetch_accepts_instances_from_one_database(clean, prefetch_databases):
    Author, _, objs = prefetch_databases
    await orm.prefetch(objs[1:], Author.books)
    assert [[b.title for b in obj.books.cached] for obj in objs[1:]] == [[], ["B-d2"]]
    alice, bob, _ = await seed()
    await orm.prefetch([alice, bob], User.posts)
    assert [p.title for p in alice.posts.cached] == ["draft", "hit", "quiet"]
    # Implicit default and explicit default resolve to the same database.
    explicit = await User.objects.using(clean).require().get(User.id == alice.id)
    await orm.prefetch([bob, explicit], User.posts)
    assert [p.title for p in explicit.posts.cached] == ["draft", "hit", "quiet"]


# -- model metadata for factories (item 12) ---------------------------------------------------


def test_describe_gives_fields_relations_and_unique_keys():
    from decimal import Decimal

    from blog.models import Role

    post = orm.describe(Post)
    assert post["name"] == "Post" and post["table"] == "posts" and post["primary_key"] == ["id"]
    fields = {f["name"]: f for f in post["fields"]}
    assert fields["id"]["default"] == "database" and fields["id"]["primary_key"]
    assert fields["title"] == {
        "name": "title",
        "column": "title",
        "type": "string",
        "python_type": str,
        "element_type": None,
        "nullable": False,
        "array": False,
        "enum": None,
        "max_length": 200,
        "primary_key": False,
        "unique": False,
        "default": None,
        "insert": True,
    }
    relations = {r["name"]: r for r in post["relations"]}
    assert relations["author"]["kind"] == "belongs_to" and relations["author"]["target"] is User
    assert relations["author"]["from"] == ["author_id"] and not relations["author"]["nullable"]
    assert relations["tags"]["kind"] == "many_to_many" and relations["tags"]["through"] is PostTag
    assert orm.describe(PostTag)["unique"] == [("id",), ("post_id", "tag_id")]
    assert orm.describe(User)["unique"] == [("id",), ("email",)]
    profile = {f["name"]: f for f in orm.describe(Profile)["fields"]}
    assert profile["role"]["enum"] is Role and profile["role"]["python_type"] is Role
    assert profile["balance"]["python_type"] is Decimal
    assert profile["links"]["array"] and profile["links"]["element_type"] is str
    comment = {f["name"]: f for f in orm.describe(Comment)["fields"]}
    assert comment["author_id"]["nullable"]


async def make(model, n=[0], **values):
    """A factory built only on describe() and objects.insert(): required fields get
    generated values, a required to-one relation gets a new related row."""
    info = orm.describe(model)
    n[0] += 1
    for rel in info["relations"]:
        if (
            rel["kind"] == "belongs_to"
            and not rel["nullable"]
            and rel["name"] not in values
            and not any(f in values for f in rel["from"])
        ):
            values[rel["name"]] = await make(rel["target"])
    samples = {
        str: lambda f: f"{f['name']}-{n[0]}"[: f["max_length"] or None],
        int: lambda f: n[0],
        bool: lambda f: False,
    }
    for f in info["fields"]:
        if f["insert"] and f["default"] is None and not f["nullable"] and f["name"] not in values:
            if not any(f["name"] in r["from"] and r["name"] in values for r in info["relations"]):
                values[f["name"]] = samples[f["python_type"]](f)
    return await model.objects.insert(**values)


async def test_a_factory_needs_only_describe_and_insert(clean):
    post = await make(Post)
    assert post.title.startswith("title-") and post.views == 0
    author = await User.objects.get(User.id == post.author_id)
    assert author.email.startswith("email-")
    tag_link = await make(PostTag, post=post)
    assert tag_link.post_id == post.id


# -- columns through to-one load() paths (F11) ------------------------------------------------------


async def test_only_through_a_to_one_path(clean):
    await seed()
    qs = Comment.objects.load(Comment.body, Comment.post.title)
    assert qs.inspect() == orm.QueryInspection(1, False, (orm.StatementInspection(False, 1, 0, 0, 0),))
    sql = qs.sql()
    assert '"title"' in sql and '"views"' not in sql and '"published"' not in sql
    comment = await Comment.objects.insert(post=(await Post.objects.get(Post.title == "hit")), body="x")
    (c,) = await qs.filter(Comment.id == comment.id)
    assert c.body == "x" and c.post.title == "hit" and c.post.pk is not None
    with pytest.raises(orm.NotLoaded):
        c.post.views
    with pytest.raises(orm.NotLoaded):
        c.created_at


async def test_load_a_path_column_keeps_the_root_whole(clean):
    # Q2.5.4: only a column of the model itself makes it partial
    _, _, posts = await seed()
    await Comment.objects.insert(post=posts[0], body="x")
    query = Comment.objects.load(Comment.post.author.name)
    assert query.inspect() == orm.QueryInspection(1, False, (orm.StatementInspection(False, 2, 0, 0, 0),))
    (c,) = await query
    assert c.post.author.name == "Alice" and c.body == "x" and c.post.title == posts[0].title
    with pytest.raises(orm.NotLoaded):
        c.post.author.email
    (c,) = await Comment.objects.load(Comment.id, Comment.post.author.name)
    with pytest.raises(orm.NotLoaded):
        c.body
    # a bare path to the same model asks for all its fields
    (c,) = await Comment.objects.load(Comment.post.author.name, Comment.post.author)
    assert c.post.author.email == "alice@example.com"


async def test_load_a_column_through_a_to_many_path(clean, observe_queries):
    await seed()
    query = User.objects.load(User.posts.title).order_by(User.id)
    assert query.inspect() == orm.QueryInspection(
        2,
        True,
        (
            orm.StatementInspection(False, 0, 0, 0, 0),
            orm.StatementInspection(True, 0, 0, 0, 0),
        ),
    )
    with observe_queries(clean) as events:
        users = await query
    assert len(events) == 2
    assert sorted(p.title for p in users[0].posts.cached) == ["draft", "hit", "quiet"]
    with pytest.raises(orm.NotLoaded):
        users[0].posts.cached[0].views
    assert users[0].name == "Alice"


# -- qs1 | qs2, indexed labels, orm.column ----------------------------------------------


async def test_or_of_query_sets(clean):
    await seed()
    popular = Post.objects.order_by(Post.id).filter(Post.views >= 80)
    drafts = Post.objects.filter(~Post.published)
    assert [p.title for p in await (popular | drafts)] == ["draft", "hit"]
    assert len(await (popular | Post.objects)) == 4
    with pytest.raises(orm.QueryError, match="one filter"):
        popular.filter(Post.views < 100) | drafts
    with pytest.raises(orm.QueryError, match="sets order"):
        drafts | popular
    with pytest.raises(orm.QueryError, match="sliced"):
        popular[:1] | drafts


async def _users_with_unpublished_posts_loaded():
    await seed()
    return await User.objects.order_by(User.id).load(User.posts.objects.filter(~Post.published))


async def test_or_of_a_relation_set_ignores_the_loaded_rows(clean):
    users = await _users_with_unpublished_posts_loaded()
    rows = await (users[0].posts | Post.objects.filter(Post.published))
    assert sorted(p.title for p in rows) == ["bob", "draft", "hit", "quiet"]


async def test_or_of_a_relation_set_is_a_plain_query_set(clean):
    users = await _users_with_unpublished_posts_loaded()
    qs = users[0].posts | Post.objects.filter(Post.published)
    assert type(qs) is QuerySet
    assert not hasattr(qs, "_relation")


async def test_or_of_a_many_to_many_set_ignores_the_loaded_rows(clean):
    _, _, posts = await seed()
    tags = await Tag.objects.insert_many([{"name": n} for n in "abc"]).returning()
    await posts[1].tags.add(*tags)
    post = (await Post.objects.filter(Post.id == posts[1].id).load(Post.tags.objects.filter(Tag.name == "a")))[0]
    rows = await (post.tags | Tag.objects.filter(Tag.name == "c"))
    assert sorted(t.name for t in rows) == ["a", "b", "c"]
    assert type(post.tags | Tag.objects) is QuerySet


async def test_or_with_a_relation_set_on_the_right_reads_the_database(clean):
    users = await _users_with_unpublished_posts_loaded()
    rows = await (Post.objects.filter(Post.published) | users[0].posts)
    assert sorted(p.title for p in rows) == ["bob", "draft", "hit", "quiet"]


async def test_or_of_two_via_query_sets_raises_query_error(clean):
    a = User.posts.objects.filter(Post.views > 1)
    b = User.posts.objects.filter(Post.views < 100)
    with pytest.raises(orm.QueryError, match="via"):
        a | b


async def test_or_of_two_filtered_relation_sets_names_the_relation(clean):
    alice, _, _ = await seed()
    with pytest.raises(orm.QueryError, match=r"posts.*filter\(a \| b\)"):
        alice.posts.filter(Post.views > 1) | alice.posts.filter(Post.views < 100)


async def test_or_keeps_the_custom_query_set_class(clean, custom):
    users = await _users_with_unpublished_posts_loaded()
    qs = users[0].posts | Post.objects.filter(Post.published)
    assert type(qs).__name__ == "PostQueries"
    assert sorted(p.title for p in await qs.popular(60)) == ["bob", "hit"]


async def test_prefetch_one_stores_a_row_or_none(clean, observe_queries):
    await seed()
    await User.objects.insert(email="carol@example.com", name="Carol")
    query = User.objects.order_by(User.id).load(
        User.posts.objects.order_by(-Post.views)[0].label("best"),
        User.posts.objects.order_by(-Post.views)[1].label("second"),
        User.posts.objects.order_by(-Post.views)[-1].label("last_post"),
    )
    assert query.inspect() == orm.QueryInspection(
        4,
        True,
        (
            orm.StatementInspection(False, 0, 0, 0, 0),
            orm.StatementInspection(True, 0, 0, 1, 0),
            orm.StatementInspection(True, 0, 0, 1, 0),
            orm.StatementInspection(True, 0, 0, 1, 0),
        ),
    )
    with observe_queries(clean) as events:
        users = await query
    assert len(events) == 4
    assert [u.second.title if u.second else None for u in users] == ["draft", None, None]
    assert [u.last_post.title if u.last_post else None for u in users] == ["quiet", "bob", None]
    assert [u.best.title if u.best else None for u in users] == ["hit", "bob", None]
    with pytest.raises(TypeError):
        User.objects.load(User.posts.objects[0])
    with pytest.raises(orm.QueryError, match="to-many") as one_error:
        await Post.objects.load(Post.author.objects[0].label("writer"))
    assert "to_attr" not in str(one_error.value)
    with pytest.raises(ValueError, match="field, relation or member"):
        User.objects.load(User.posts.objects.label("name"))
    with pytest.raises(ValueError, match="identifier"):
        User.posts.objects.label("_x")
    with pytest.raises(TypeError):
        await User.posts.objects[0]
    with pytest.raises(TypeError):
        User.posts.objects.label("old", one=True)


async def test_column_from_a_dotted_path(clean):
    await seed()
    assert repr(orm.column(Comment, "post.author.name")) == repr(Comment.post.author.name)
    names = await User.objects.filter(orm.column(User, "posts.title").startswith("hi")).select(User.name).scalars()
    assert names == ["Alice"]
    assert [p.title for p in await Post.objects.order_by(-orm.column(Post, "views"))][:1] == ["hit"]
    with pytest.raises(LookupError, match="Post has no relation 'nope'"):
        orm.column(Post, "nope.title")
    with pytest.raises(LookupError, match="User has no field 'nope'"):
        orm.column(Post, "author.nope")


# -- mutation regressions (G10) -------------------------------------------------------------


@pytest.mark.parametrize("slot", ["db", "loads", "without_defaults"])
def test_or_rejects_right_side_state(slot):
    if slot == "db":
        right = Post.objects.using(object())
    elif slot == "loads":
        # Isolate the slot: load() also derives state checked by separate guards.
        right = Post.objects._clone(_loads=(Post.author,))
    else:
        right = Post.objects.without_defaults()
    with pytest.raises(orm.QueryError, match=f"sets {slot}"):
        Post.objects | right


@pytest.mark.parametrize("side", ["left", "right"])
def test_or_rejects_offset_without_limit(side):
    plain = Post.objects.filter(Post.published)
    sliced = Post.objects[1:]
    with pytest.raises(orm.QueryError, match="sliced"):
        (sliced | plain) if side == "left" else (plain | sliced)


def test_or_rejects_a_different_model():
    with pytest.raises(TypeError, match="query sets of Post"):
        Post.objects | User.objects


@pytest.mark.parametrize("model, relation", [(Post, "posts"), (Tag, "tags")])
async def test_changing_query_set_class_discards_cached_relation_class(clean, model, relation):
    class Before(QuerySet):
        def marker(self):
            return "before"

    class After(QuerySet):
        def marker(self):
            return "after"

    alice, _, posts = await seed()
    owner = alice if relation == "posts" else posts[0]
    try:
        use_query_set(model, Before)
        assert getattr(owner, relation).marker() == "before"
        use_query_set(model, After)
        assert getattr(owner, relation).marker() == "after"
    finally:
        use_query_set(model, QuerySet)


@pytest.mark.parametrize("base", [orm.RelatedSet, orm.ManyRelatedSet])
def test_query_set_class_rejects_relation_subclasses(base):
    class RelationQueries(base):
        __slots__ = ()

    with pytest.raises(TypeError, match="subclass of orm.QuerySet"):
        use_query_set(Post, RelationQueries)


async def test_prefetch_rejects_a_model_subclass(clean):
    alice, _, _ = await seed()

    class SpecialUser(User, registry=orm.Registry()):
        pass

    special = object.__new__(SpecialUser)
    special.__dict__.update(alice.__dict__)
    with pytest.raises(TypeError, match="one model"):
        await orm.prefetch([alice, special], User.posts)


def test_column_rejects_unknown_own_field():
    with pytest.raises(LookupError, match="Post has no field 'nope'"):
        orm.column(Post, "nope")


def test_describe_primary_key_is_unique():
    fields = {f["name"]: f for f in orm.describe(Post)["fields"]}
    assert fields["id"]["unique"] is True


def test_describe_reports_client_default():
    models = orm.loads("model ClientItem {\n id Int @id\n n Int @client_default(3)\n}", registry=orm.Registry())
    fields = {f["name"]: f for f in orm.describe(models["ClientItem"])["fields"]}
    assert fields["n"]["default"] == "client"


def test_describe_autoincrement_remains_insertable():
    fields = {f["name"]: f for f in orm.describe(Post)["fields"]}
    assert fields["id"]["insert"] is True


def test_describe_array_python_type_is_list():
    fields = {f["name"]: f for f in orm.describe(Profile)["fields"]}
    assert fields["links"]["python_type"] is list


async def test_or_with_unfiltered_left_returns_all_rows(clean):
    await seed()
    rows = await (Post.objects | Post.objects.filter(Post.title == "missing"))
    assert {p.title for p in rows} == {"draft", "hit", "quiet", "bob"}


def test_describe_optional_belongs_to_is_nullable():
    relations = {r["name"]: r for r in orm.describe(Comment)["relations"]}
    assert relations["author"]["nullable"] is True


async def test_prefetch_rejects_own_column(clean):
    alice, _, _ = await seed()
    with pytest.raises(TypeError, match="own model"):
        await orm.prefetch([alice], User.name)


def test_get_cannot_be_labeled_for_loading():
    with pytest.raises(TypeError, match=r"get\(\) can't be loaded per parent; use \[0\]\.label\(name\)"):
        User.posts.objects.get().label("unique_post")


def test_unlabeled_one_row_load_suggests_label():
    with pytest.raises(TypeError, match=r"a one-row query in load\(\) needs \.label\(name\)"):
        User.objects.load(User.posts.objects[0])


def test_one_row_repr():
    assert repr(User.posts.objects[0]).startswith("<OneRowQuery <QuerySet Post")
