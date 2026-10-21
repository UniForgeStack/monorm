"""The null rule on relation paths: `rel != x`, a NOT NULL or nullable target column, the path-level
rule, and the consumers (join `on`, aggregate `filter=`) — on PostgreSQL and SQLite."""

import pytest
from blog.models import Comment, Post, User

import orm
from conftest import DATABASE_URL
from orm import QueryError, func, not_

SCHEMA = """
model Writer {
  id    BigInt  @id @default(autoincrement())
  email String
  nick  String?
  notes Note[]
  @@map("np_writer")
}

model Note {
  id        BigInt  @id @default(autoincrement())
  writer_id BigInt?
  writer    Writer? @relation(fields: [writer_id], references: [id])
  score     Int?
  label     String  @default("")
  @@map("np_note")
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def rows(request):
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
    Writer, Note = m["Writer"], m["Note"]
    w1, w2, w3 = await Writer.objects.using(db).insert_many(
        [{"email": "a", "nick": "n"}, {"email": "b", "nick": "m"}, {"email": "c", "nick": None}]
    ).returning()
    n1, n2, n3, n4, n5 = await Note.objects.using(db).insert_many(
        [
            {"writer_id": w1.id, "score": 1, "label": "a"},
            {"writer_id": w2.id, "score": 2, "label": "a"},
            {"writer_id": None, "score": 3, "label": "a"},
            {"writer_id": w3.id, "score": 1, "label": "c"},
            {"writer_id": w1.id, "score": None, "label": "z"},
        ]
    ).returning()

    async def notes(condition):
        return {r.id for r in await Note.objects.using(db).filter(condition)}

    async def writers(condition):
        return {r.id for r in await Writer.objects.using(db).filter(condition)}

    yield Writer, Note, (w1, w2, w3), (n1, n2, n3, n4, n5), notes, writers
    await db.drop_tables()
    await db.close()


def ids(*objs):
    return {o.id for o in objs}


async def test_a_to_one_relation_ne_keeps_the_rows_with_no_related_row(rows):
    Writer, Note, (w1, w2, w3), (n1, n2, n3, n4, n5), notes, _ = rows
    assert await notes(Note.writer == w1) == ids(n1, n5)
    assert await notes(Note.writer != w1) == ids(n2, n3, n4)
    assert await notes(~(Note.writer == w1)) == ids(n2, n3, n4)
    assert await notes(~(Note.writer != w1)) == ids(n1, n5)
    assert await notes(not_(Note.writer == w1)) == ids(n2, n3, n4)
    assert await notes(Note.writer != None) == ids(n1, n2, n4, n5)  # noqa: E711


async def test_a_column_ne_keeps_the_column_rule(rows):
    _, Note, (w1, *_), (n1, n2, n3, n4, n5), notes, _ = rows
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        await notes(Note.writer_id != w1.id)
    assert await notes((Note.writer_id != w1.id).or_null()) == ids(n2, n3, n4)
    assert await notes((Note.writer_id != w1.id).and_not_null()) == ids(n2, n4)


async def test_a_not_null_target_column_is_not_a_nullable_value(rows):
    Writer, Note, _, (n1, n2, n3, n4, n5), notes, _ = rows
    assert await notes(Note.writer.email == "a") == ids(n1, n5)
    assert await notes(Note.writer.email != "a") == ids(n2, n3, n4)
    assert await notes(Note.writer.email.not_in(["a"])) == ids(n2, n3, n4)
    assert await notes(~(Note.writer.email == "a")) == ids(n2, n3, n4)
    assert await notes((Note.writer.email != "a").or_null()) == ids(n2, n3, n4)
    assert await notes((Note.writer.email != "a").and_not_null()) == ids(n2, n4)


async def test_a_to_one_ne_against_a_root_column_keeps_the_rows_with_no_related_row(rows):
    _, Note, _, (n1, n2, n3, n4, n5), notes, _ = rows
    assert await notes(Note.writer.email != Note.label) == ids(n2, n3, n5)
    assert await notes(Note.label != Note.writer.email) == ids(n2, n3, n5)
    assert await notes(~(Note.writer.email == Note.label)) == ids(n2, n3, n5)
    assert await notes(Note.writer.email.not_in([Note.label])) == ids(n2, n3, n5)


async def test_a_nullable_target_column_takes_the_strict_rule_and_the_methods(rows):
    _, Note, _, (n1, n2, n3, n4, n5), notes, _ = rows
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        await notes(Note.writer.nick != "n")
    with pytest.raises(QueryError, match="under negation"):
        await notes(~(Note.writer.nick == "n"))
    assert await notes(Note.writer.nick == "n") == ids(n1, n5)
    assert await notes((Note.writer.nick != "n").or_null()) == ids(n2, n3, n4)
    assert await notes((Note.writer.nick != "n").and_not_null()) == ids(n2)
    assert await notes(~(Note.writer.nick == "n").or_null()) == ids(n2)
    assert await notes(~(Note.writer.nick == "n").and_not_null()) == ids(n2, n3, n4)


async def test_the_path_level_rule_covers_the_missing_row_and_every_nullable_column(rows):
    _, Note, (w1, *_), (n1, n2, n3, n4, n5), notes, _ = rows
    assert await notes(Note.writer.or_null().nick != "n") == ids(n2, n3, n4)
    assert await notes(Note.writer.and_not_null().nick != "n") == ids(n2)
    assert await notes(~(Note.writer.and_not_null().nick == "n")) == ids(n2, n3, n4)
    assert await notes(~(Note.writer.or_null().nick == "n")) == ids(n2)
    assert await notes(Note.writer.or_null() != w1) == ids(n2, n3, n4)
    assert await notes(Note.writer.and_not_null() != w1) == ids(n2, n4)
    assert await notes(Note.writer.and_not_null() == w1) == ids(n1, n5)


async def test_a_method_on_one_comparison_wins_over_the_path(rows):
    _, Note, _, (n1, n2, n3, n4, n5), notes, _ = rows
    assert await notes((Note.writer.or_null().nick != "n").and_not_null()) == ids(n2)
    assert await notes((Note.writer.and_not_null().nick != "n").or_null()) == ids(n2, n3, n4)


async def test_the_methods_apply_per_related_row_on_a_to_many_path(rows):
    Writer, _, (w1, w2, w3), _, _, writers = rows
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        await writers(Writer.notes.score != 1)
    assert await writers((Writer.notes.score != 1).or_null()) == ids(w1, w2)
    assert await writers((Writer.notes.score != 1).and_not_null()) == ids(w2)
    assert await writers(Writer.notes.or_null().score != 1) == ids(w1, w2)
    assert await writers(Writer.notes.and_not_null().score != 1) == ids(w2)
    assert await writers(~(Writer.notes.score == 2).and_not_null()) == ids(w1, w3)
    assert await writers(~(Writer.notes.score == 2).or_null()) == ids(w3)


def test_the_check_runs_in_a_join_on_and_an_aggregate_filter():
    totals = Post.objects.select(Post.author_id, func.sum(Post.views).label("views")).group_by(Post.author_id).cte("totals")
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        Comment.objects.join(totals, Comment.author_id != 1)
    assert Comment.objects.join(totals, (Comment.author_id != 1).or_null()).sql()
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        func.count(filter=Comment.author_id != 1)
    assert func.count(filter=(Comment.author_id != 1).and_not_null()) is not None


def test_the_strict_check_does_not_run_in_a_case_or_a_select():
    assert Comment.objects.select(func.case((Comment.author_id != 1, 1), default=0).label("flag")).sql()


def test_a_to_one_not_in_a_subquery_and_a_mixed_path_flip_in_sql():
    sub = User.objects.select(User.email).filter(User.name == "x")
    sql = Comment.objects.filter(Comment.author.email.not_in(sub)).sql()
    assert 'WHERE NOT EXISTS(SELECT 1 FROM "users" AS "t1"' in sql and '"t1"."email" IN (SELECT' in sql
    sql = Post.objects.filter(Post.comments.author.email != "x").sql()
    assert '"t1"."post_id" = "posts"."id" AND (NOT EXISTS(SELECT 1 FROM "users" AS "t2"' in sql


async def test_a_null_method_on_a_to_one_hop_after_a_to_many_hop_applies_per_related_row(rows):
    Writer, _, (w1, w2, w3), _, _, writers = rows
    # The Writer fixture is circular, so every writer matches `email == "a"` through its own notes; the SQL shape is the check.
    assert await writers((Writer.notes.writer.email == "a").or_null()) == ids(w1)
    assert await writers(Writer.notes.writer.or_null().nick == "n") == ids(w1, w3)
    assert await writers(Writer.notes.writer.and_not_null().nick == "n") == ids(w1)


def test_the_null_methods_on_a_to_one_hop_after_a_to_many_hop_make_the_rule_inside_the_exists():
    for cond in (
        (Post.comments.author.email == "a").or_null(),
        Post.comments.author.or_null().email == "a",
    ):
        sql = Post.objects.filter(cond).sql()
        assert 'NOT EXISTS(SELECT 1 FROM "comments" AS "t1"' in sql and sql.count('"t2"."email" IS NOT NULL') == 1
    sql = Post.objects.filter(Post.comments.author.and_not_null().email == "a").sql()
    assert "NOT EXISTS" not in sql and """("t2"."email" = 'a' AND "t2"."email" IS NOT NULL)""" in sql


def test_a_path_rule_applies_in_a_case_condition_and_a_select_value():
    cond = Comment.author.or_null().name == "x"
    for sql in (
        Comment.objects.select(func.case((cond, 1), default=0).label("flag")).sql(),
        Comment.objects.select(cond.label("flag")).sql(),
    ):
        assert 'NOT EXISTS(SELECT 1 FROM "users"' in sql and '"name" IS NOT NULL' in sql
    # A path rule that is not set keeps the plain value and never raises in these places.
    assert "NOT EXISTS" not in Comment.objects.select(func.case((Comment.author.name == "x", 1), default=0).label("flag")).sql()
