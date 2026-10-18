"""Partial output shapes and compiled defaults on both supported databases."""

import asyncio
import json
import os

import pytest
import orm
from orm import _native

QUERY_DEFAULTS = "query-defaults" in json.loads(_native.native_artifact()).get("capabilities", [])


def schema(dialect, defaults=True):
    def field(name, kind="string", **flags):
        return {"name": name, "column": name, "type": kind, **flags}

    return {
        "dialect": dialect,
        "models": [
            {
                "name": "SelectedAuthor",
                "table": "selection03_authors",
                "fields": [
                    field("name"),
                    field("id", "int", primary_key=True, auto_increment=True),
                    field("visible", "bool", default=True),
                    field("bio", "text"),
                    field("note", nullable=True),
                ],
                "relations": [
                    {"name": "books", "kind": "many", "target": "SelectedBook", "from": "id", "to": "author_id"}
                ],
            },
            {
                "name": "SelectedBook",
                "table": "selection03_books",
                "fields": [
                    field("title"),
                    field("author_id", "int"),
                    field("id", "int", primary_key=True, auto_increment=True),
                    field("isbn", nullable=True),
                ],
                "relations": [
                    {
                        "name": "author",
                        "kind": "one",
                        "target": "SelectedAuthor",
                        "from": "author_id",
                        "to": "id",
                        "foreign_key": True,
                        "on_delete": "cascade",
                    }
                ],
            },
        ],
        "behavior": {
            "schema_contract": 2,
            "read_defaults": [
                {
                    "source": "orm.query-defaults",
                    "name": "query",
                    "model": "SelectedAuthor",
                    "filter": {"t": "col", "path": [], "name": "visible"},
                    "fields": ["name", "note"],
                },
                {
                    "source": "orm.query-defaults",
                    "name": "query",
                    "model": "SelectedBook",
                    "fields": ["title"],
                    "load": [["author"]],
                },
            ],
        }
        if defaults
        else {},
    }


async def open_db(dialect, defaults):
    registry = orm.Registry()
    classes = orm.define(schema(dialect, defaults), registry=registry)
    url = (
        "sqlite://:memory:"
        if dialect == "sqlite"
        else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    )
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, classes["SelectedAuthor"], classes["SelectedBook"]


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_partial_selection(dialect):
    db, Author, Book = await open_db(dialect, defaults=False)
    a, b = Author.objects.using(db), Book.objects.using(db)
    try:
        author = await a.insert(name="ann", bio="large", note=None)
        await a.insert(name="bob", bio="large")
        await b.insert(title="one", author_id=author.pk)
        partial = await a.load(Author.name, Author.note).order_by(Author.id).first()
        assert partial.to_dict() == {"name": "ann", "note": None}
        assert partial.pk == author.pk
        with pytest.raises(orm.NotLoaded):
            partial.bio
        with pytest.raises(TypeError):
            a.load(Author.name, Author.name)
        await partial.update(name="changed")
        assert partial.to_dict() == {"name": "changed", "note": None}
        await partial.refresh(Author.bio)
        assert partial.to_dict() == {"name": "changed", "note": None, "bio": "large"}
        assert [o.name async for batch in a.load(Author.name).batches(1) for o in batch] == ["changed", "bob"]
        assert (await partial.books.using(db).get()).title == "one"
        full = await a.load().order_by(Author.id).first()
        assert "_orm_internal" not in full.__dict__ and full.bio == "large"
        book = await b.load(Book.title).get()
        with pytest.raises(orm.NotLoaded):
            book.author
        assert (await b.load(Book.author).load(Book.title).get()).author.name == "changed"
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_selection_defaults(dialect):
    db, Author, Book = await open_db(dialect, defaults=True)
    a, b = Author.objects.using(db), Book.objects.using(db)
    try:
        with pytest.raises(ValueError):
            await a.insert(name="missing required excluded field")
        visible = await a.insert(name="visible", bio="large", note=None)
        hidden = await a.insert(name="hidden", bio="large", visible=False)
        await b.insert(title="one", author_id=visible.pk)
        await b.insert(title="two", author_id=hidden.pk)
        partial = await a.get()
        assert partial.to_dict() == {"name": "visible", "note": None}
        assert partial.pk == visible.pk
        for name in ("id", "bio", "visible"):
            with pytest.raises(orm.NotLoaded):
                getattr(partial, name)
        assert "bio" not in a.sql()
        assert await a.count() == 1 and await a.exists()
        assert await a.without_defaults().count() == 2
        assert await a.without_defaults().filter(Author.name == "visible").count() == 1
        full = await a.load().get()
        assert full.bio == "large"
        explicit = await a.load(Author.note, Author.name).get()
        assert explicit.to_dict() == {"name": "visible", "note": None}
        await explicit.refresh()
        assert explicit.to_dict() == {"name": "visible", "note": None}
        await explicit.refresh(Author.bio)
        assert explicit.to_dict() == {"name": "visible", "note": None, "bio": "large"}
        await partial.update(name="changed")
        assert partial.to_dict() == {"name": "changed", "note": None}
        joined = await b.order_by(Book.id)
        assert len(joined) == 2 and joined[0].author.name == "changed" and joined[1].author is None
        assert joined[0].to_dict() == {"title": "one"}
        assert joined[0].author.to_dict() == {"name": "changed", "note": None}
        with pytest.raises(orm.NotLoaded):
            joined[0].author_id
        await joined[1].update(author_id=visible.pk)
        with pytest.raises(orm.NotLoaded):
            joined[1].author
        await joined[1].update(author_id=hidden.pk)
        assert joined[1]._field_value("author_id") == hidden.pk
        assert await b.filter(Book.author.name == "hidden").update_many([{"id": joined[1].pk, "title": "t"}]) == 0
        bypassed = await b.without_defaults().load(Book.author).order_by(Book.id)
        assert bypassed[0].author.name == "changed" and bypassed[0].author.visible is True
        assert bypassed[1].author.name == "hidden"
        assert await a.in_bulk() == {visible.pk: partial}
        changed_return = await a.load(Author.name).update(name="again").returning()
        assert changed_return[0].to_dict() == {"name": "again"}
        default_return = await a.filter(Author.id == visible.pk).update(note="n").returning()
        assert default_return[0].to_dict() == {"name": "again", "note": "n"}
        cleared = await b.without_related().first()
        with pytest.raises(orm.NotLoaded):
            cleared.author
        prefetched = await a.load(Author.books).get()
        assert prefetched.books.cached[0].title == "one"
        bypassed_prefetch = await a.without_defaults().load(Author.books).filter(Author.id == visible.pk).get()
        assert (
            bypassed_prefetch.books.cached[0].title == "one"
            and bypassed_prefetch.books.cached[0].author_id == visible.pk
        )
        assert await a.filter(Author.books.title == "one").count() == 1
        written = await a.load().update(visible=False).returning()
        assert len(written) == 1 and written[0].visible is False
        assert await a.count() == 0
        assert await a.without_defaults().update_many([{"id": hidden.pk, "name": "bulk"}]) == 1
        assert await a.update_many([{"id": hidden.pk, "name": "excluded"}]) == 0
        assert await a.delete() == 0
        assert await a.without_defaults().filter(Author.id == hidden.pk).delete() == 1
    finally:
        await db.drop_tables()
        await db.close()


def test_current_filter_lists_filters_with_their_bypass_state():
    classes = orm.define(schema("sqlite", True), registry=orm.Registry())
    Author, Book = classes["SelectedAuthor"], classes["SelectedBook"]
    visible = {"t": "col", "path": [], "name": "visible"}
    assert Author.objects.current_filter() == [
        {"source": "orm.query-defaults", "name": "query", "expr": visible, "active": True}
    ]
    assert Author.objects.without_defaults("query").current_filter()[0]["active"] is False
    assert Author.objects.without_defaults().current_filter()[0]["active"] is False
    assert Book.objects.current_filter() == []
    with pytest.raises(Exception, match=r'unknown default "nope"; available defaults: \["query"\]'):
        Author.objects.without_defaults("nope").current_filter()


ORDERED = """
datasource db {
  provider = "sqlite"
}
model Topic {
  id Int @id @default(autoincrement())
  rank Int?
  name String
  notes Note[]
  @@map("order09_topics")
  @@query.order("-rank nulls last", "id")
}
model Note {
  id Int @id @default(autoincrement())
  topic_id Int
  body String
  topic Topic @relation(fields: [topic_id], references: [id], onDelete: Cascade)
  @@map("order09_notes")
  @@query.defaults(order: ["-body"])
}
"""


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_default_order(dialect):
    registry = orm.Registry()
    models = orm.loads(
        ORDERED.replace('"sqlite"', '"postgresql"' if dialect == "postgres" else '"sqlite"'), registry=registry
    )
    Topic, Note = models["Topic"], models["Note"]
    url = (
        "sqlite://:memory:"
        if dialect == "sqlite"
        else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    )
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    t, n = Topic.objects.using(db), Note.objects.using(db)

    def names(rows):
        return [r.name for r in rows]

    try:
        for name, rank in [("a", 1), ("b", None), ("c", 3), ("d", 3)]:
            await t.insert(name=name, rank=rank)
        assert names(await t.all()) == ["c", "d", "a", "b"]
        assert (await t.first()).name == "c" and (await t.last()).name == "b"
        assert (await t[1]).name == "d"
        assert (await t[-1]).name == "b"
        assert names(await t.order_by(Topic.name.desc())) == ["d", "c", "b", "a"]
        assert names(await t.order_by(Topic.rank.asc(nulls="first"), Topic.id)) == ["b", "a", "c", "d"]
        assert (
            "ORDER BY" not in t.without_defaults().sql()
            and "ORDER BY" not in t.filter(Topic.id > 0).without_defaults().sql()
        )
        assert (await t.without_defaults().first()).name == "a"
        assert await t.count() == 4 and await t.exists()
        assert [names(b) async for b in t.batches(2)] == [["a", "b"], ["c", "d"]]
        page = await t.paginate(first=3)
        assert names(page.items) == ["c", "d", "a"]
        assert names((await t.paginate(first=3, after=page.next_cursor)).items) == ["b"]
        assert names((await t.without_defaults().paginate(first=3)).items) == ["a", "b", "c"]
        assert (await t.from_(t.cte("all_topics")).first()).name == "a"
        # Postgres rejects UPDATE ... ORDER BY, so these also show that writes take no default order.
        assert await t.filter(Topic.name == "none").update(name="x") == 0
        assert await t.filter(Topic.name == "none").delete() == 0
        c = await t.get(Topic.name == "c")
        for body in ["x", "z", "y"]:
            await n.insert(topic_id=c.pk, body=body)
        assert [x.body for x in await n.all()] == ["z", "y", "x"]
        loaded = await t.load(Topic.notes).get(Topic.name == "c")
        assert [x.body for x in await loaded.notes] == ["z", "y", "x"]
        own = await t.load(Topic.notes.objects.order_by(Note.body)).get(Topic.name == "c")
        assert [x.body for x in await own.notes] == ["x", "y", "z"]
        sliced = await t.load(Topic.notes.objects.all()[:2]).get(Topic.name == "c")
        assert [x.body for x in await sliced.notes] == ["z", "y"]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.fixture(params=["sqlite", "postgres"])
async def one_row_topics(request):
    dialect = request.param
    registry = orm.Registry()
    models = orm.loads(
        ORDERED.replace('@@query.defaults(order: ["-body"])', "")
        .replace('@@query.order("-rank nulls last", "id")', "")
        .replace('"sqlite"', '"postgresql"' if dialect == "postgres" else '"sqlite"'),
        registry=registry,
    )
    Topic = models["Topic"]
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ["ORM_TEST_DATABASE_URL"]
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    t = Topic.objects.using(db)
    try:
        await t.insert_many([{"name": name, "rank": rank} for name, rank in [("c", 3), ("b", 2), ("a", 1)]])
        yield db, Topic, t
    finally:
        await db.drop_tables()
        await db.close()


async def test_one_row_indices_and_default_order(one_row_topics):
    _, Topic, t = one_row_topics
    assert (await t[0]).name == "c"
    assert (await t[1]).name == "b"
    assert (await t[-1]).name == "a"
    assert (await t.order_by(Topic.name)[0]).name == "a"
    assert (await t.without_defaults()[0]).name == "c"
    assert (await t[1:3][1]).name == "a"
    assert (await t[1:2].first()).name == "b"


async def test_one_row_missing_and_required(one_row_topics):
    _, Topic, t = one_row_topics
    assert await t[1:2][1] is None
    assert await t[99] is None
    assert await t.filter(Topic.name == "missing").first() is None
    with pytest.raises(Topic.DoesNotExist):
        await t.require()[99]
    with pytest.raises(Topic.DoesNotExist):
        await t.filter(Topic.name == "missing").require().last()


async def test_one_row_laziness_and_fresh_awaits(one_row_topics, observe_queries):
    db, Topic, t = one_row_topics
    with observe_queries(db) as events:
        first, last, get, indexed = t.first(), t.last(), t.get(Topic.name == "b"), t[1]
        assert isinstance(first, orm.OneRowQuery)
        assert events == []
        assert first.sql() and first.inspect().statements
        assert events == []
        assert (await first).name == "c"
        assert (await last).name == "a"
        assert (await get).name == "b"
        assert (await indexed).name == "b"
        assert len(events) == 4
        assert (await first).name == "c"
        assert len(events) == 5
    with pytest.raises(Topic.MultipleObjectsReturned):
        await t.get()


async def test_one_row_create_task(one_row_topics):
    _, Topic, t = one_row_topics
    query = t.first()
    assert isinstance(query, orm.OneRowQuery)
    assert (await asyncio.create_task(query)).name == "c"
    assert (await query).name == "c"
    assert (await asyncio.create_task(t.get(Topic.name == "b"))).name == "b"
    cancelled_query = t.first()
    task = asyncio.create_task(cancelled_query)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert (await cancelled_query).name == "c"
    with pytest.raises(Topic.DoesNotExist):
        await asyncio.create_task(t.filter(Topic.name == "missing").require().first())


async def test_one_row_index_errors(one_row_topics):
    _, _, t = one_row_topics
    for sliced in [t[:2], t[1:], t.limit(0), t.offset(1)]:
        with pytest.raises(ValueError, match="limit or offset"):
            sliced.last()
        with pytest.raises(ValueError, match="limit or offset"):
            sliced[-1]
    with pytest.raises(ValueError):
        t[-2]
    with pytest.raises(orm.QueryError, match="param"):
        t[orm.param("index")]
    limited = t.limit(orm.param("limit"))
    with pytest.raises(orm.QueryError, match="param"):
        limited[0]
    with pytest.raises(orm.QueryError, match="param"):
        limited.first()
    with pytest.raises(orm.QueryError, match="param"):
        limited.get()
    with pytest.raises(ValueError, match="limit or offset"):
        limited.last()
    with pytest.raises(orm.QueryError, match="param"):
        await limited.prepare().first(limit=0)
    with pytest.raises(orm.QueryError, match="param"):
        await limited.prepare().get(limit=0)


async def test_one_row_prepared(one_row_topics, observe_queries):
    db, Topic, t = one_row_topics
    for make in [lambda q: q.first(), lambda q: q.last(), lambda q: q[0], lambda q: q.get()]:
        query = make(t.filter(Topic.name == orm.param("name")))
        prepared = query.prepare()
        with observe_queries(db) as events:
            assert prepared.sql(name="b") and prepared.inspect(name="b").statements
            assert events == []
            assert (await prepared(name="b")).name == "b"
            assert await prepared(name="missing") is None
            assert len(events) == 2
    with pytest.raises(Topic.MultipleObjectsReturned):
        await t.get().prepare()()
    with pytest.raises(Topic.DoesNotExist):
        await t.filter(Topic.name == "missing").require().first().prepare()()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
def test_default_order_options_have_one_source():
    both = ORDERED.replace(
        '@@query.defaults(order: ["-body"])', '@@query.defaults(order: ["-body"])\n  @@query.order("id")'
    )
    with pytest.raises(
        Exception, match=r"Note: order is set by both @@query\.defaults\(order:\) at .* and @@query\.order at"
    ):
        orm.loads(both, registry=orm.Registry())
    with pytest.raises(Exception, match=r"order column \"nope\" is not a field"):
        orm.loads(ORDERED.replace('"-body"', '"nope"'), registry=orm.Registry())
    with pytest.raises(
        Exception, match=r'Topic: order column "rank" can be NULL: write "-rank nulls first" or "-rank nulls last"'
    ):
        orm.loads(ORDERED.replace('"-rank nulls last"', '"-rank"'), registry=orm.Registry())
    with pytest.raises(Exception, match=r'elsewhere quote it: "-name"'):
        orm.loads(ORDERED.replace('"-rank nulls last", "id"', "-name"), registry=orm.Registry())
    with pytest.raises(Exception, match=r'order column "k" is json, which the database can\'t order'):
        orm.loads(
            ORDERED.replace("  rank Int?\n", "  rank Int?\n  k Json @db.Json\n")
            .replace('"-rank nulls last", "id"', '"k"')
            .replace('"sqlite"', '"postgresql"'),
            registry=orm.Registry(),
        )


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="requires the query-defaults artifact")
async def test_a_joined_model_brings_its_related_default():
    def field(name, kind="string", **flags):
        return {"name": name, "column": name, "type": kind, **flags}

    ir = {
        "dialect": "sqlite",
        "models": [
            {"name": "JAuthor", "table": "j_authors", "fields": [field("id", "int", primary_key=True), field("name")]},
            {
                "name": "JBook",
                "table": "j_books",
                "fields": [field("id", "int", primary_key=True), field("author_id", "int")],
                "relations": [
                    {
                        "name": "author",
                        "kind": "one",
                        "target": "JAuthor",
                        "from": "author_id",
                        "to": "id",
                        "foreign_key": True,
                    }
                ],
            },
            {
                "name": "JShelf",
                "table": "j_shelves",
                "fields": [field("id", "int", primary_key=True), field("book_id", "int")],
                "relations": [
                    {
                        "name": "book",
                        "kind": "one",
                        "target": "JBook",
                        "from": "book_id",
                        "to": "id",
                        "foreign_key": True,
                    }
                ],
            },
        ],
        "behavior": {
            "schema_contract": 2,
            "read_defaults": [
                {"source": "orm.query-defaults", "name": "query", "model": "JBook", "load": [["author"]]}
            ],
        },
    }
    registry = orm.Registry()
    classes = orm.define(ir, registry=registry)
    Author, Book, Shelf = classes["JAuthor"], classes["JBook"], classes["JShelf"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        await Author.objects.using(db).insert(id=1, name="ann")
        await Book.objects.using(db).insert(id=1, author_id=1)
        await Shelf.objects.using(db).insert(id=1, book_id=1)
        # JBook's default joins its author also when JBook is itself joined.
        shelf = await Shelf.objects.using(db).load(Shelf.book).get()
        assert shelf.book.author.name == "ann"
    finally:
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="requires the query-defaults artifact")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_a_missing_reference_is_none_only_when_the_engine_filtered_the_join(request, dialect):
    db, A, B, P = await open_unset(request, dialect, True)
    try:
        for Model in (B, P):
            rows = Model.objects.using(db).order_by(Model.id)
            for path in (Model.author, Model.author.objects.as_prefetch()):
                loaded = await rows.load(path).all()
                assert [row.author for row in loaded][1:] == ([None, None, None] if Model is B else [None, None])
            first, dangling, *rest = await rows.without_defaults().load(Model.author).all()
            assert first.author.name == "ann"
            with pytest.raises(orm.NotLoaded):
                dangling.author
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="requires the query-defaults artifact")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_the_target_flag_does_not_depend_on_model_order(request, dialect):
    # Put the book before the filtered author in the public schema.
    db, A, B, P = await open_unset(request, dialect, True, target_last=True)
    try:
        books = B.objects.using(db).order_by(B.id)
        for path in (B.author, B.author.objects.as_prefetch()):
            assert [b.author for b in await books.load(path).all()][1:] == [None, None, None]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_load_path_with_columns_unites_the_default_selection(dialect):
    db, Author, Book = await open_db(dialect, defaults=True)
    a, b = Author.objects.using(db), Book.objects.using(db)
    try:
        ann = await a.insert(name="ann", bio="large", note="n")
        await b.insert_many([{"title": "one", "author_id": ann.pk, "isbn": "i1"}])
        # joined: the default selection (name, note) plus the named bio
        for items in ((Book.author, Book.author.bio), (Book.author.bio, Book.author)):
            book = await b.load(*items).get()
            assert book.author.to_dict() == {"name": "ann", "note": "n", "bio": "large"}
        # a bare path beside a set with own columns: the default selection plus those columns
        book = await b.load(Book.author, Book.author.objects.load(Author.bio)).get()
        assert book.author.to_dict() == {"name": "ann", "note": "n", "bio": "large"}
        # IN query for a to-many relation, both orders
        for items in ((Author.books, Author.books.isbn), (Author.books.isbn, Author.books)):
            author = await a.load(*items).get()
            assert [x.to_dict() for x in await author.books] == [{"title": "one", "isbn": "i1"}]
        # guards: a column alone stays partial
        book = await b.load(Book.author.bio).get()
        assert book.author.to_dict() == {"bio": "large"}
        author = await a.load(Author.books.isbn).get()
        assert [x.to_dict() for x in await author.books] == [{"isbn": "i1"}]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
async def test_aggregate_to_one_tail_applies_the_target_default_in_the_join():
    ir = schema("postgres")
    book = ir["models"][1]
    book["fields"].append({"name": "shelf_id", "column": "shelf_id", "type": "int", "nullable": True})
    ir["models"].append(
        {
            "name": "SelectedShelf",
            "table": "selection03_shelves",
            "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": True, "auto_increment": True},
            ],
            "relations": [{"name": "books", "kind": "many", "target": "SelectedBook", "from": "id", "to": "shelf_id"}],
        }
    )
    registry = orm.Registry()
    classes = orm.define(ir, registry=registry)
    db = await orm.connect(os.environ["ORM_TEST_DATABASE_URL"], registry=registry, default=False)
    Author, Book, Shelf = classes["SelectedAuthor"], classes["SelectedBook"], classes["SelectedShelf"]
    try:
        await db.drop_tables()
        await db.create_tables()
        shelf = await Shelf.objects.using(db).insert()
        shown, hidden = (
            await Author.objects.using(db)
            .without_defaults()
            .insert_many([{"name": "shown", "bio": "x"}, {"name": "hidden", "bio": "x", "visible": False}])
            .returning()
        )
        await Book.objects.using(db).insert_many(
            [
                {"title": "a", "author_id": shown.pk, "shelf_id": shelf.pk},
                {"title": "b", "author_id": hidden.pk, "shelf_id": shelf.pk},
            ]
        )
        # The hidden author is no row of the join, but its book stays an element: the default sits in the ON.
        row = (
            await Shelf.objects.using(db)
            .select(
                orm.func.array_agg(Shelf.books.author.name, order_by=Shelf.books.title),
                orm.func.count(Shelf.books.author),
            )
            .first()
        )
        assert tuple(row) == (["shown", None], 1)
    finally:
        await db.drop_tables()
        await db.close()


UNSET = """
datasource db {
  provider = "sqlite"
}
model UAuthor {
  id Int @id
  name String
  active Boolean @default(true)
  books UBook[]
  profile UProfile?
  @@map("g2_authors")
  //FILTER
}
model UBook {
  id Int @id
  author_id Int?
  author UAuthor? @relation(fields: [author_id], references: [id])
  @@map("g2_books")
}
model UProfile {
  id Int @id
  author_id Int @unique
  author UAuthor @relation(fields: [author_id], references: [id])
  @@map("g2_profiles")
}
"""
FILTER = '@@query.filter("active", "active == true")'


async def open_unset(request, dialect, filtered, target_last=False, composite=False):
    source = UNSET.replace("//FILTER", FILTER if filtered else "").replace(
        '"sqlite"', '"postgresql"' if dialect == "postgres" else '"sqlite"'
    )
    if composite:
        source = source.replace("id Int @id\n  name", "id Int\n  region Int @default(1)\n  @@id([id, region])\n  name")
        source = source.replace("author_id Int?", "author_region Int? @default(1)\n  author_id Int?")
        source = source.replace(
            "author_id Int @unique",
            "author_region Int @default(1)\n  @@unique([author_id, author_region])\n  author_id Int @unique",
        )
        source = source.replace(
            "fields: [author_id], references: [id]", "fields: [author_id, author_region], references: [id, region]"
        )
    if target_last:
        start, end = source.index("model UAuthor"), source.index("model UBook")
        source = source[:start] + source[end:] + source[start:end]
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    url = (
        "sqlite://:memory:"
        if dialect == "sqlite"
        else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    )
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    if dialect == "sqlite":
        await db.execute("PRAGMA foreign_keys = OFF")
    else:
        await db.execute("ALTER TABLE g2_books DROP CONSTRAINT IF EXISTS g2_books_author_id_fkey")
        await db.execute("ALTER TABLE g2_profiles DROP CONSTRAINT IF EXISTS g2_profiles_author_id_fkey")
        if composite:
            for table in ("g2_books", "g2_profiles"):
                await db.execute(f"ALTER TABLE {table} DROP CONSTRAINT IF EXISTS {table}_author_id_author_region_fkey")
    A, B, P = models["UAuthor"], models["UBook"], models["UProfile"]
    await (
        A.objects.using(db)
        .without_defaults()
        .insert_many(
            [
                {"id": 1, "name": "ann", "active": True},
                {"id": 2, "name": "bob", "active": False},
                {"id": 3, "name": "cy", "active": True},
            ]
        )
    )
    # Book 1 has an author, 2 a dangling key, 3 a NULL key, 4 a hidden author.
    await B.objects.using(db).insert_many(
        [{"id": 1, "author_id": 1}, {"id": 2, "author_id": 99}, {"id": 3, "author_id": None}, {"id": 4, "author_id": 2}]
    )
    await P.objects.using(db).insert_many(
        [{"id": 1, "author_id": 1}, {"id": 2, "author_id": 99}, {"id": 3, "author_id": 2}]
    )
    return db, A, B, P


@pytest.fixture(params=["sqlite", "postgres"])
async def unset(request):
    db, A, B, P = await open_unset(request, request.param, False)
    try:
        yield db, A, B, P
    finally:
        await db.drop_tables()
        await db.close()


@pytest.fixture
def base_getter(monkeypatch):
    """The base getter: no reference adapter, as in a build without reference loading."""
    import orm.model

    monkeypatch.setattr(orm.model, "_reference_adapter", None)


def state(book):
    try:
        return book.author
    except orm.NotLoaded:
        return "unset"


async def test_base_getter_prefetch_of_an_unfiltered_dangling_key_is_not_loaded(base_getter, unset):
    db, A, B, P = unset
    books = await B.objects.using(db).order_by(B.id).load(B.author.objects.as_prefetch()).all()
    assert [state(b) for b in books[:3]] == [books[0].author, "unset", None]
    assert books[0].author.name == "ann"


async def test_base_getter_join_of_an_unfiltered_dangling_key_is_not_loaded(base_getter, unset):
    db, A, B, P = unset
    books = await B.objects.using(db).order_by(B.id).load(B.author).all()
    assert [state(b) for b in books][1:3] == ["unset", None]


async def test_base_getter_instance_load_of_an_unfiltered_dangling_key_is_not_loaded(base_getter, unset):
    db, A, B, P = unset
    book = await B.objects.using(db).get(B.id == 2)
    book.__dict__["author"] = None  # a stale value the load must not keep
    await book.load(B.author, reload=True)
    assert state(book) == "unset"
    await book.load(B.author.objects.as_prefetch(), reload=True)
    assert state(book) == "unset"


async def test_base_getter_keeps_none_for_has_one_and_label(base_getter, unset):
    db, A, B, P = unset
    ann = await A.objects.using(db).order_by(A.id).load(A.profile).get(A.id == 3)
    assert ann.profile is None
    books = await B.objects.using(db).order_by(B.id).load(B.author.objects.as_prefetch().label("writer")).all()
    assert books[1].__dict__["writer"] is None


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_base_getter_filtered_target_is_none_and_bypassed_prefetch_is_not_loaded(base_getter, request, dialect):
    db, A, B, P = await open_unset(request, dialect, True)
    try:
        books = await B.objects.using(db).order_by(B.id).load(B.author.objects.as_prefetch()).all()
        assert [state(b) for b in books[1:]] == [None, None, None]
        books = await B.objects.using(db).without_defaults().order_by(B.id).load(B.author.objects.as_prefetch()).all()
        assert [state(b) for b in books][1:] == ["unset", None, books[3].author]
        assert books[3].author.name == "bob"
    finally:
        await db.drop_tables()
        await db.close()


async def test_prefetch_of_a_nullable_dangling_key_is_not_loaded(unset):
    db, A, B, P = unset
    books = await B.objects.using(db).order_by(B.id).load(B.author.objects.as_prefetch()).all()
    assert [state(b) for b in books[:3]] == [books[0].author, "unset", None]


async def test_load_as_prefetch_of_a_nullable_dangling_key_is_not_loaded(unset):
    db, A, B, P = unset
    book = await B.objects.using(db).get(B.id == 2)
    await book.load(B.author.objects.as_prefetch())
    assert state(book) == "unset"


async def test_adapter_join_and_prefetch_of_required_and_nullable_dangling_keys(unset):
    db, A, B, P = unset
    for Model in (B, P):
        for path in (Model.author, Model.author.objects.as_prefetch()):
            row = await Model.objects.using(db).load(path).get(Model.id == 2)
            with pytest.raises(orm.NotLoaded):
                row.author


async def test_plain_load_of_a_required_dangling_key_raises_integrity_error(unset):
    db, A, B, P = unset
    row = await P.objects.using(db).get(P.id == 2)
    with pytest.raises(orm.IntegrityError):
        await row.load(P.author)


async def test_plain_load_of_a_nullable_dangling_key_keeps_none(unset):
    # F2: pin the current adapter behavior pending an owner decision.
    db, A, B, P = unset
    row = await B.objects.using(db).get(B.id == 2)
    await row.load(B.author)
    assert row.author is None


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
@pytest.mark.parametrize("composite", [False, True])
@pytest.mark.parametrize("filtered", [False, True])
@pytest.mark.parametrize("method", ["join", "prefetch", "instance"])
async def test_absent_reference_message_tracks_the_loaded_key(request, dialect, composite, filtered, method):
    if filtered and not QUERY_DEFAULTS:
        pytest.skip("needs a native build with the query-defaults feature")
    db, A, B, P = await open_unset(request, dialect, filtered, composite=composite)
    message = "the key is set and no row of the target has it"
    try:
        for Model in (B, P):
            rows = Model.objects.using(db).without_defaults()
            if method != "instance":
                rows = rows.load(Model.author if method == "join" else Model.author.objects.as_prefetch())
            row = await rows.get(Model.id == 2)
            if method == "instance":
                await row.load(Model.author.objects.without_defaults().as_prefetch(), reload=True)
            with pytest.raises(orm.NotLoaded, match=message):
                row.author
            source = "author_region" if composite else "author_id"
            row.__dict__[source] = 2 if composite else 98
            with pytest.raises(orm.NotLoaded) as error:
                row.author
            assert message not in str(error.value)
            row = await Model.objects.using(db).get(Model.id == 2)
            with pytest.raises(orm.NotLoaded) as error:
                row.author
            assert message not in str(error.value)
            await row.load(Model.author.objects.without_defaults().as_prefetch(), reload=True)
            with pytest.raises(orm.NotLoaded, match=message):
                row.author
            await row.update(author_id=3)
            with pytest.raises(orm.NotLoaded) as error:
                row.author
            assert message not in str(error.value)
            await row.load(Model.author.objects.as_prefetch(), reload=True)
            assert row.author.name == "cy"
        row = await B.objects.using(db).without_defaults().load(B.author).get(B.id == 3)
        assert row.author is None
        if filtered:
            row = await B.objects.using(db).load(B.author).get(B.id == 4)
            assert row.author is None
    finally:
        await db.drop_tables()
        await db.close()


async def test_unknown_bypass_name_lists_the_defaults_of_the_model():
    db, Author, _ = await open_db("sqlite", defaults=False)
    try:
        qs = Author.objects.using(db)
        assert qs.without_defaults("a")._without_defaults == ("a",)
        assert qs.without_defaults("a").without_defaults("b", "a")._without_defaults == ("a", "b")
        assert qs.without_defaults("a").without_defaults()._without_defaults is True
        assert qs.without_defaults().without_defaults("a")._without_defaults is True
        with pytest.raises(orm.QueryError, match=r'unknown default "nope"; available defaults: \[\]'):
            await qs.without_defaults("nope").count()
        with pytest.raises(orm.QueryError, match='unknown default "nope"'):
            await qs.without_defaults("nope").filter(Author.name == "x").update(name="y")
        with pytest.raises(orm.QueryError, match='unknown default "nope"'):
            await qs.without_defaults("nope").filter(Author.name == "x").delete()
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_bypass_by_name_keeps_the_other_defaults(dialect):
    db, Author, Book = await open_db(dialect, defaults=True)
    a = Author.objects.using(db)
    try:
        await a.insert_many([{"name": "visible", "bio": "x"}, {"name": "hidden", "bio": "x", "visible": False}])
        assert await a.without_defaults("query").count() == 2
        with pytest.raises(orm.QueryError, match=r'unknown default "nope"; available defaults: \["query"\]'):
            await a.without_defaults("nope").count()
        full = await a.without_defaults("query").order_by(Author.id).first()
        assert full.bio == "x"
    finally:
        await db.drop_tables()
        await db.close()
