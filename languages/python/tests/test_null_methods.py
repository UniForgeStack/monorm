"""`.or_null()` / `.and_not_null()`: a NULL row kept or dropped, on PostgreSQL and SQLite."""

import pytest
from blog.models import Comment, Post, User

import orm
from conftest import DATABASE_URL
from orm import QueryError
from orm.expr import differs

SCHEMA = """
model Reading {
  id    BigInt  @id @default(autoincrement())
  score Int?
  name  String?
  tag   String
  @@map("nm_reading")
}

model Item {
  shop  BigInt
  id    BigInt
  lines Line[]
  @@id([shop, id])
  @@map("nm_item")
}

model Line {
  id      BigInt  @id @default(autoincrement())
  shop    BigInt?
  item_id BigInt?
  item    Item?   @relation(fields: [shop, item_id], references: [shop, id])
  @@map("nm_line")
}

model Note {
  id     BigInt      @id @default(autoincrement())
  labels Label[]     @relation(through: NoteLabel)
  links  NoteLabel[]
  @@map("nm_note")
}

model Label {
  id    BigInt      @id @default(autoincrement())
  notes Note[]      @relation(through: NoteLabel)
  links NoteLabel[]
  @@map("nm_label")
}

model NoteLabel {
  id       BigInt @id @default(autoincrement())
  note_id  BigInt
  label_id BigInt?
  note     Note   @relation(fields: [note_id], references: [id])
  label    Label? @relation(fields: [label_id], references: [id])
  @@map("nm_note_label")
}
"""


def where(qs) -> str:
    return qs.sql().split(" WHERE ", 1)[1]


def test_method_binds_before_not():
    c = ~(Comment.author_id > 20).or_null()
    assert where(Comment.objects.filter(c)) == 'NOT ("comments"."author_id" > 20 OR "comments"."author_id" IS NULL)'
    c = ~(Comment.author_id > 20).and_not_null()
    assert where(Comment.objects.filter(c)) == 'NOT ("comments"."author_id" > 20 AND "comments"."author_id" IS NOT NULL)'


def test_sql_of_each_form():
    col = Comment.author_id
    assert where(Comment.objects.filter((col == 1).or_null())) == (
        '"comments"."author_id" = 1 OR "comments"."author_id" IS NULL'
    )
    assert where(Comment.objects.filter(col.in_([1, 2]).and_not_null())) == (
        '"comments"."author_id" IN (1, 2) AND "comments"."author_id" IS NOT NULL'
    )
    assert where(Comment.objects.filter(col.between(1, 5).or_null())) == (
        '("comments"."author_id" >= 1 AND "comments"."author_id" <= 5) OR "comments"."author_id" IS NULL'
    )


def test_a_precedence_trap_raises_instead_of_guessing():
    with pytest.raises(TypeError, match="applies to one comparison"):
        (~(Comment.id > 20)).or_null()
    with pytest.raises(TypeError, match="applies to one comparison"):
        ((Comment.author_id > 1) | (Comment.author_id < 0)).or_null()
    with pytest.raises(TypeError, match="already a NULL test"):
        Comment.author_id.is_null().and_not_null()
    with pytest.raises(TypeError, match=r"Comment.author_id IS NOT NULL"):
        (Comment.author_id != None).or_null()  # noqa: E711


def test_a_not_null_value_raises():
    with pytest.raises(QueryError, match="NOT NULL"):
        (User.email == "a").or_null()
    with pytest.raises(QueryError, match="NOT NULL"):
        User.email.like("a%").and_not_null()


def test_a_nullable_column_reached_through_a_to_many_relation_takes_the_method():
    assert where(Post.objects.filter((Post.comments.author_id == 1).or_null())).count('"author_id" IS NULL') == 1
    with pytest.raises(QueryError, match="NOT NULL"):
        (Post.comments.id == 1).or_null()


def test_a_computed_value_is_not_checked_yet():
    with pytest.raises(TypeError, match="applies to a plain column"):
        (Comment.author_id + 1 > 5).or_null()


@pytest.fixture(params=["sqlite", "postgres"])
async def models(request):
    registry = orm.Registry()
    source = SCHEMA if request.param == "postgres" else 'datasource db {\n  provider = "sqlite"\n}\n' + SCHEMA
    m = orm.loads(source, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else DATABASE_URL
    try:
        db = await orm.connect(url, registry=registry, default=False)
    except orm.DatabaseError as e:
        pytest.skip(f"database not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    yield db, m["Reading"], m["Item"], m["Line"], m
    await db.drop_tables()
    await db.close()


async def test_a_null_row_is_kept_or_dropped(models):
    db, Reading, *_ = models
    a, b, null = await Reading.objects.using(db).insert_many(
        [
            {"score": 10, "name": "ab", "tag": "t"},
            {"score": 30, "name": "cd", "tag": "t"},
            {"score": None, "name": None, "tag": "t"},
        ]
    ).returning()
    s, n = Reading.score, Reading.name
    # (condition, the non-NULL rows it matches)
    cases = [
        (s == 10, {a}, False),
        (s != 10, {b}, True),
        (s < 20, {a}, False),
        (s > 20, {b}, False),
        (s.in_([10]), {a}, False),
        (s.not_in([10]), {b}, True),
        (s.between(5, 15), {a}, False),
        (n.like("a%"), {a}, False),
        (n.ilike("A%"), {a}, False),
        (n.contains("d"), {b}, False),
    ]
    both = {a, b}

    async def ids(condition):
        return {r.id for r in await Reading.objects.using(db).filter(condition)}

    for c, matched, negative in cases:
        m = {r.id for r in matched}
        rest = {r.id for r in both - matched}
        nid = {null.id}
        label = repr(c)
        if negative:
            with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows, and_not_null\(\) drops them"):
                await ids(c)
        else:
            assert await ids(c) == m, label
        with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation, and_not_null\(\) keeps"):
            ~c
        assert await ids(c.or_null()) == m | nid, label
        assert await ids(c.and_not_null()) == m, label
        assert await ids(~c.or_null()) == rest, label
        assert await ids(~c.and_not_null()) == rest | nid, label
    assert {r.id for r in await Reading.objects.using(db).exclude((s > 20).and_not_null())} == {a.id, null.id}
    assert {r.id for r in await Reading.objects.using(db).exclude((s > 20).or_null())} == {a.id}


async def test_a_composite_key_has_one_is_null_per_column(models):
    db, _, Item, Line, _ = models
    a, _ = await Item.objects.using(db).insert_many([{"shop": 1, "id": 1}, {"shop": 2, "id": 1}]).returning()
    one, two, null, half = await Line.objects.using(db).insert_many(
        [
            {"item": a},
            {"shop": 2, "item_id": 1},
            {"shop": None, "item_id": None},
            {"shop": 1, "item_id": None},
        ]
    ).returning()

    async def ids(condition):
        return {r.id for r in await Line.objects.using(db).filter(condition)}

    eq = Line.item == a
    ne = Line.item != a
    assert where(Line.objects.filter(eq.or_null())).count(" IS NULL") == 2
    assert await ids(eq) == {one.id}
    assert await ids(eq.or_null()) == {one.id, null.id, half.id}
    assert await ids(eq.and_not_null()) == {one.id}
    assert await ids(ne) == {two.id, null.id, half.id}
    assert await ids(~eq) == {two.id, null.id, half.id}
    assert await ids(~ne) == {one.id}
    assert await ids(ne.or_null()) == {two.id, null.id, half.id}
    assert await ids(ne.and_not_null()) == {two.id}
    assert await ids(~eq.or_null()) == {two.id}
    assert await ids(~eq.and_not_null()) == {two.id, null.id, half.id}


def test_a_constant_in_keeps_its_column():
    col = Comment.author_id
    assert where(Comment.objects.filter(col.in_([]).or_null())) == '(FALSE) OR "comments"."author_id" IS NULL'
    assert where(Comment.objects.filter(col.not_in([]).or_null())) == '(TRUE) OR "comments"."author_id" IS NULL'
    assert where(Comment.objects.filter(col.in_([]).and_not_null())) == '(FALSE) AND "comments"."author_id" IS NOT NULL'
    assert where(Comment.objects.filter(col.not_in([]).and_not_null())) == '(TRUE) AND "comments"."author_id" IS NOT NULL'
    with pytest.raises(QueryError, match="NOT NULL"):
        User.email.in_([]).or_null()


def test_a_method_covers_every_nullable_column_of_a_comparison():
    ref = Comment.author_id
    between = ~Comment.id.between(ref, 5).or_null()
    assert where(Comment.objects.filter(between)).count('"comments"."author_id" IS NULL') == 1
    computed = ~(orm.func.coalesce(Comment.id, 0) == ref).or_null()
    assert where(Comment.objects.filter(computed)).endswith('"comments"."author_id" IS NULL)')


def test_a_condition_reads_back_in_the_messages():
    col = Comment.author_id
    for condition, text in [
        (col.is_null(), "Comment.author_id IS NULL"),
        (col.is_not_null(), "Comment.author_id IS NOT NULL"),
        (User.email.like("a%"), "User.email LIKE 'a%'"),
        (User.email.ilike("a%"), "User.email ILIKE 'a%'"),
        (col.in_([]), "FALSE"),
        (col.not_in([]), "TRUE"),
    ]:
        assert repr(condition) == text
    with pytest.raises(TypeError, match=r"User\.email LIKE 'a%'"):
        ((User.email.like("a%")) | (User.email.like("b%"))).or_null()
    with pytest.raises(TypeError, match=r"not to ~Comment\.author_id IS NULL"):
        orm.not_(col.is_null()).and_not_null()


def test_differs_keeps_a_null_row_and_passes_a_null_test_through():
    col = Comment.author_id
    assert where(Comment.objects.filter(differs(col, 1))) == (
        '"comments"."author_id" <> 1 OR "comments"."author_id" IS NULL'
    )
    assert where(Comment.objects.filter(differs(col, [1, 2]))) == (
        '"comments"."author_id" NOT IN (1, 2) OR "comments"."author_id" IS NULL'
    )
    assert where(Comment.objects.filter(differs(col, None))) == '"comments"."author_id" IS NOT NULL'
    assert where(User.objects.filter(differs(User.email, "a"))) == '"users"."email" <> \'a\''


async def test_set_on_a_nullable_link_target_clears_every_link_and_keeps_none_by_accident(models):
    db, _, _, _, m = models
    Note, Label, NoteLabel = m["Note"], m["Label"], m["NoteLabel"]
    note, other = await Note.objects.using(db).insert_many([{}, {}]).returning()
    a, b = await Label.objects.using(db).insert_many([{}, {}]).returning()
    await NoteLabel.objects.using(db).insert_many(
        [
            {"note_id": note.id, "label_id": a.id},
            {"note_id": note.id, "label_id": b.id},
            {"note_id": note.id, "label_id": None},
            {"note_id": other.id, "label_id": a.id},
        ]
    )

    async def links(of):
        rows = await NoteLabel.objects.using(db).filter(NoteLabel.note_id == of.id)
        return sorted((r.label_id for r in rows), key=lambda v: (v is None, v))

    note = await Note.objects.using(db).get(Note.id == note.id)
    await note.labels.set([a])
    assert await links(note) == [a.id]
    await note.labels.set([])
    assert await links(note) == []
    assert await links(other) == [a.id]
