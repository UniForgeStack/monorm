"""What a write returns: the default selection, or the columns named in returning(...)."""

import pytest

import monorm
from conftest import DATABASE_URL
from test_selection_defaults import open_db

COMPOSITE = """
model ReturnItem {
  shop  BigInt
  id    BigInt
  title String
  note  String?
  @@id([shop, id])
  @@map("ret_item")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def authors(request):
    db, Author, Book = await open_db(request.param, defaults=True)
    try:
        yield Author, Book, Author.objects.using(db), Book.objects.using(db)
    finally:
        await db.drop_tables()
        await db.close()


def loaded(row):
    return row.to_dict()


async def test_each_write_returns_the_default_selection_without_columns(authors):
    Author, Book, a, b = authors
    one = await a.insert(name="one", bio="long", note="n")
    assert loaded(one) == {"name": "one", "note": "n"}
    many = await a.insert_many([{"name": "two", "bio": "x"}, {"name": "three", "bio": "y"}]).returning()
    assert [loaded(r) for r in many] == [{"name": "two", "note": None}, {"name": "three", "note": None}]
    updated = await a.filter(Author.name == "one").update(note="m").returning()
    assert [loaded(r) for r in updated] == [{"name": "one", "note": "m"}]
    many_updated = await a.update_many([{"id": one.pk, "note": "k"}]).returning()
    assert [loaded(r) for r in many_updated] == [{"name": "one", "note": "k"}]
    deleted = await a.filter(Author.name == "three").delete().returning()
    assert [loaded(r) for r in deleted] == [{"name": "three", "note": None}]
    assert await a.filter(Author.name == "two").update(note="c") == 1


async def test_each_write_returns_the_named_columns_and_the_key(authors):
    Author, Book, a, b = authors
    one = await a.insert(name="one", bio="long").returning(Author.bio)
    assert loaded(one) == {"bio": "long"} and one.pk == 1
    many = await a.insert_many([{"name": "two", "bio": "x"}, {"name": "three", "bio": "y"}]).returning(Author.bio)
    assert [loaded(r) for r in many] == [{"bio": "x"}, {"bio": "y"}]
    updated = await a.filter(Author.name == "one").update(note="m").returning(Author.note)
    assert [loaded(r) for r in updated] == [{"note": "m"}]
    many_updated = await a.update_many([{"id": one.pk, "note": "k"}]).returning(Author.note, Author.bio)
    assert [loaded(r) for r in many_updated] == [{"note": "k", "bio": "long"}]
    deleted = await a.filter(Author.name == "two").delete().returning(Author.bio)
    assert [loaded(r) for r in deleted] == [{"bio": "x"}]
    for row in (one, *many, *updated, *many_updated, *deleted):
        with pytest.raises(monorm.NotLoaded):
            row.name


async def test_a_field_hidden_by_the_defaults_is_not_loaded_even_when_supplied(authors):
    Author, Book, a, b = authors
    row = await a.insert(name="hidden", bio="supplied", visible=False)
    for name in ("bio", "visible"):
        with pytest.raises(monorm.NotLoaded):
            getattr(row, name)
    assert row.name == "hidden"


async def test_without_defaults_restores_the_hidden_fields(authors):
    Author, Book, a, b = authors
    row = await a.without_defaults().insert(name="all", bio="supplied", visible=False)
    assert (row.bio, row.visible, row.id) == ("supplied", False, 1)
    rows = await a.without_defaults().insert_many([{"name": "b", "bio": "x"}]).returning()
    assert (rows[0].bio, rows[0].visible) == ("x", True)
    changed = await a.without_defaults().filter(Author.name == "all").update(note="n").returning()
    assert changed[0].bio == "supplied"
    gone = await a.without_defaults().filter(Author.name == "all").delete().returning()
    assert gone[0].visible is False


async def test_an_explicit_column_wins_over_the_defaults(authors):
    Author, Book, a, b = authors
    row = await a.insert(name="x", bio="b", visible=False).returning(Author.visible)
    assert loaded(row) == {"visible": False}
    with pytest.raises(monorm.NotLoaded):
        row.note
    rows = await a.without_defaults().filter(Author.id == row.pk).update(bio="c").returning(Author.name)
    assert loaded(rows[0]) == {"name": "x"}


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_the_write_statement_returns_only_the_selected_columns(dialect, observe_queries):
    db, Author, Book = await open_db(dialect, defaults=True)
    a = Author.objects.using(db)
    try:

        def returned(events):
            (write,) = [e.sql for e in events if "RETURNING" in e.sql]
            return write.split("RETURNING", 1)[1]

        def columns(tail):
            return {name for name in ("name", "note", "bio", "visible") if f'"{name}"' in tail}

        with observe_queries(db) as events:
            await a.insert(name="one", bio="long")
        assert columns(returned(events)) == {"name", "note"}
        with observe_queries(db) as events:
            await a.insert(name="two", bio="long").returning(Author.bio)
        assert columns(returned(events)) == {"bio"}
        with observe_queries(db) as events:
            await a.without_defaults().insert(name="three", bio="long")
        assert columns(returned(events)) == {"name", "note", "bio", "visible"}
        with observe_queries(db) as events:
            await a.filter(Author.name == "one").update(note="m").returning(Author.note)
        assert columns(returned(events)) == {"note"}
        with observe_queries(db) as events:
            await a.filter(Author.name == "one").delete().returning()
        assert columns(returned(events)) == {"name", "note"}
    finally:
        await db.drop_tables()
        await db.close()


async def test_insert_many_returns_the_rows_in_input_order(authors):
    Author, Book, a, b = authors
    names = [f"n{i}" for i in range(7, 0, -1)]
    rows = await a.insert_many([{"name": n, "bio": "b"} for n in names]).returning(Author.name)
    assert [r.name for r in rows] == names
    assert [r.pk for r in rows] == sorted(r.pk for r in rows)


async def test_upsert_returns_the_named_columns_and_skipped_rows_stay_out(authors):
    Author, Book, a, b = authors
    first = await a.insert(name="u", bio="1").returning(Author.bio)
    assert loaded(first) == {"bio": "1"}
    skipped = await a.insert(id=first.pk, name="u2", bio="2").on_conflict(Author.id, update=False).returning(Author.bio)
    assert skipped is None
    updated = await a.insert(id=first.pk, name="u3", bio="3").on_conflict(Author.id, update=True).returning(Author.bio)
    assert loaded(updated) == {"bio": "3"} and updated.pk == first.pk
    rows = (
        await a.insert_many([{"id": first.pk, "name": "x", "bio": "b"}, {"name": "y", "bio": "4"}])
        .on_conflict(Author.id, update=False)
        .returning(Author.bio)
    )
    assert [loaded(r) for r in rows] == [{"bio": "4"}]


async def test_update_many_returns_the_matched_rows(authors):
    Author, Book, a, b = authors
    rows = await a.insert_many([{"name": "a", "bio": "b"}, {"name": "b", "bio": "b"}]).returning()
    changed = await a.update_many([{"id": r.pk, "note": f"n{r.pk}"} for r in rows]).returning(Author.note)
    assert sorted(r.note for r in changed) == [f"n{r.pk}" for r in rows]
    assert await a.update_many([{"id": rows[0].pk, "note": "z"}]) == 1


async def test_a_write_with_columns_keeps_composite_key_fields():
    registry = monorm.Registry()
    models = monorm.loads(COMPOSITE, registry=registry)
    db = await monorm.connect(DATABASE_URL, registry=registry, default=False)
    Item = models["ReturnItem"]
    items = Item.objects.using(db)
    try:
        await db.drop_tables()
        await db.create_tables()
        row = await items.insert(shop=1, id=2, title="t", note="n").returning(Item.title)
        assert loaded(row) == {"title": "t"}
        assert row.pk == (1, 2)
        rows = await items.insert_many([{"shop": 1, "id": 3, "title": "u"}]).returning(Item.note)
        assert loaded(rows[0]) == {"note": None} and rows[0].pk == (1, 3)
        rows = await items.filter(Item.id == 3).update(title="v").returning(Item.title)
        assert loaded(rows[0]) == {"title": "v"} and rows[0].pk == (1, 3)
        rows = await items.update_many([{"shop": 1, "id": 3, "title": "w"}]).returning(Item.title)
        assert loaded(rows[0]) == {"title": "w"} and rows[0].pk == (1, 3)
        rows = await items.filter(Item.id == 3).delete().returning(Item.note)
        assert loaded(rows[0]) == {"note": None} and rows[0].pk == (1, 3)
    finally:
        await db.drop_tables()
        await db.close()


async def test_load_on_a_write_raises_and_names_returning(authors):
    Author, Book, a, b = authors
    row = await a.insert(name="x", bio="b")
    for call in (
        lambda: a.load(Author.bio).insert(name="y", bio="b"),
        lambda: a.load(Author.bio).insert_many([{"name": "y", "bio": "b"}]),
        lambda: a.load(Author.bio).filter(Author.id == row.pk).update(note="n"),
        lambda: a.load(Author.bio).filter(Author.id == row.pk).delete(),
        lambda: a.load(Author.bio).update_many([{"id": row.pk, "note": "n"}]),
    ):
        with pytest.raises(TypeError, match=r"returning\("):
            call()


async def test_a_relation_in_returning_raises_and_names_of_load(authors):
    Author, Book, a, b = authors
    for call in (
        lambda: a.insert(name="y", bio="b").returning(Author.books),
        lambda: b.insert(title="t", author_id=1).returning(Book.author),
        lambda: a.insert_many([{"name": "y", "bio": "b"}]).returning(Author.books),
        lambda: a.filter(Author.id == 1).update(note="n").returning(Author.books),
        lambda: a.filter(Author.id == 1).delete().returning(Author.books),
        lambda: a.update_many([{"id": 1, "note": "n"}]).returning(Author.books),
        lambda: a.insert(name="y", bio="b").returning(Book.title),
        lambda: a.insert(name="y", bio="b").returning(Author.books.title),
    ):
        with pytest.raises(TypeError, match=r"monorm\.of\(rows\)\.load\("):
            call()


async def test_a_copy_insert_keeps_the_count_and_refuses_returning(authors, request):
    if "sqlite" in request.node.callspec.id:
        pytest.skip("copy=True needs Postgres")
    Author, Book, a, b = authors
    assert await a.insert_many([{"name": "c", "bio": "b"}], copy=True) == 1
    with pytest.raises(AttributeError):
        a.insert_many([{"name": "d", "bio": "b"}], copy=True).returning(Author.bio)


async def test_a_returned_key_batch_loads_a_relation_afterwards(authors):
    Author, Book, a, b = authors
    people = await a.insert_many([{"name": "p", "bio": "b"}]).returning(Author.id)
    books = await b.insert_many([{"title": "t", "author_id": people[0].pk}]).returning(Book.author_id)
    await monorm.of(books).load(Book.author)
    assert books[0].author.name == "p"
