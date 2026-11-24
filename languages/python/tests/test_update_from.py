"""update() reads joined CTEs and to-one relations: it compiles to UPDATE ... FROM, on both databases."""

import os

import pytest

import monorm
from monorm import QueryError, func

SCHEMA = """
model Author {
  id Int @id
  name String
  nick String?
  post_count Int @default(0)
  posts Post[] @relation("a")
  edited Post[] @relation("e")
  @@map("uf_author")
}
model Post {
  id Int @id
  author_id Int
  editor_id Int?
  title String
  views Int @default(0)
  author_name String @default("")
  editor_name String?
  author Author @relation("a", fields: [author_id], references: [id])
  editor Author? @relation("e", fields: [editor_id], references: [id])
  @@map("uf_post")
}
"""


async def connect(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SCHEMA, registry=registry)
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    Author, Post = models["Author"], models["Post"]
    A, P = Author.objects.using(db), Post.objects.using(db)
    await A.insert_many([{"id": 1, "name": "A", "nick": "aa"}, {"id": 2, "name": "B"}, {"id": 3, "name": "C"}])
    await P.insert_many(
        [
            {"id": 1, "author_id": 1, "title": "p1", "views": 5},
            {"id": 2, "author_id": 1, "title": "p2", "views": 7},
            {"id": 3, "author_id": 2, "title": "p3", "views": 1, "editor_id": 1},
        ]
    )
    return db, Author, Post, A, P


def totals_of(Post, P):
    return P.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")


async def sql_of(db, statement):
    events = []
    remove = db.on_query(events.append)
    try:
        result = await statement
    finally:
        remove()
    return result, [e.sql for e in events]


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_recounts_from_an_aggregate_cte(provider):
    db, Author, Post, A, P = await connect(provider)
    try:
        totals = totals_of(Post, P)
        statement = A.join(totals, totals.c.author_id == Author.id).update(post_count=totals.c.n)
        count, sql = await sql_of(db, statement)
        assert count == 2  # an inner join: Carol has no posts, her row stays out
        assert len(sql) == 1 and 'UPDATE "uf_author" SET "post_count" = "totals"."n" FROM "totals" WHERE ' in sql[0]
        assert "SELECT" not in sql[0].split("UPDATE", 1)[1] and "JOIN" not in sql[0]
        assert {a.name: a.post_count for a in await A.all()} == {"A": 2, "B": 1, "C": 0}
        # A filter reads the CTE too, and RETURNING reads the updated row.
        rows = await A.join(totals, totals.c.author_id == Author.id).filter(totals.c.n > 1).update(
            post_count=totals.c.n + 10
        ).returning(Author.name, Author.post_count)
        assert [(r.name, r.post_count) for r in rows] == [("A", 12)]
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_with_an_outer_join_keeps_the_rows_without_a_match(provider):
    db, Author, Post, A, P = await connect(provider)
    try:
        totals = totals_of(Post, P)
        await A.all().update(post_count=7)
        statement = A.join(totals, totals.c.author_id == Author.id, outer=True).all().update(
            post_count=func.coalesce(totals.c.n, 0)
        )
        count, sql = await sql_of(db, statement)
        assert count == 3
        assert 'FROM "uf_author" AS "r1" LEFT JOIN "totals" ON "totals"."author_id" = "r1"."id" WHERE "uf_author"."id" = "r1"."id"' in sql[0]
        assert {a.name: a.post_count for a in await A.all()} == {"A": 2, "B": 1, "C": 0}
        # A filter on the target narrows the self-join rows.
        filtered = A.join(totals, totals.c.author_id == Author.id, outer=True).filter(Author.id == 3).update(
            post_count=func.coalesce(totals.c.n, 5)
        )
        assert await filtered == 1
        assert {a.name: a.post_count for a in await A.all()} == {"A": 2, "B": 1, "C": 5}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_reads_a_not_null_relation_with_an_inner_join(provider):
    db, Author, Post, A, P = await connect(provider)
    try:
        count, sql = await sql_of(db, P.all().update(author_name=Post.author.name))
        assert count == 3
        assert sql == [
            'UPDATE "uf_post" SET "author_name" = "j1"."name" FROM "uf_author" AS "j1" WHERE "j1"."id" = "uf_post"."author_id"'
        ]
        assert {p.title: p.author_name for p in await P} == {"p1": "A", "p2": "A", "p3": "B"}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_reads_a_nullable_relation_and_gives_null_without_one(provider):
    db, Author, Post, A, P = await connect(provider)
    try:
        await P.all().update(editor_name="old")
        statement = P.all().update(editor_name=Post.editor.name).returning(Post.id, Post.editor_name)
        rows, sql = await sql_of(db, statement)
        assert 'FROM "uf_post" AS "r1" LEFT JOIN "uf_author" AS "j2" ON "j2"."id" = "r1"."editor_id" WHERE "uf_post"."id" = "r1"."id"' in sql[0]
        assert 'RETURNING "uf_post"."id", "uf_post"."editor_name"' in sql[0]
        assert sorted((r.id, r.editor_name) for r in rows) == [(1, None), (2, None), (3, "A")]
        assert P.filter(Post.id == 1).update(editor_name=Post.editor.name).inspect().statement_count == 1
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_prepared_update_reads_a_joined_cte(provider):
    db, Author, Post, A, P = await connect(provider)
    try:
        totals = totals_of(Post, P)
        prepared = (
            A.join(totals, totals.c.author_id == Author.id, outer=True)
            .filter(Author.id > monorm.param("min"))
            .update(post_count=func.coalesce(totals.c.n, 0))
            .prepare()
        )
        assert await prepared(min=1) == 2
        assert {a.name: a.post_count for a in await A.all()} == {"A": 0, "B": 1, "C": 0}
    finally:
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_update_join_errors(provider):
    db, Author, Post, A, P = await connect(provider)
    try:
        totals = totals_of(Post, P)
        with pytest.raises(QueryError, match="a to-many relation can't give one value"):
            await A.all().update(nick=Author.posts.title)
        with pytest.raises(QueryError, match="can't run on a query set with from_"):
            await A.from_(Author.objects.cte("a")).all().update(nick="x")
        with pytest.raises(QueryError, match="can't run on a query set with from_"):
            await A.join(totals, totals.c.author_id == Author.id).all().update_many([{"id": 1, "nick": "x"}])
        # An inner join narrows the rows, so it counts as a filter; an outer join and a relation path do not.
        assert await A.join(totals, totals.c.author_id == Author.id).update(nick="n") == 2
        with pytest.raises(monorm.UnfilteredWriteError):
            A.join(totals, totals.c.author_id == Author.id, outer=True).update(nick="n")
        with pytest.raises(monorm.UnfilteredWriteError):
            P.update(author_name=Post.author.name)
    finally:
        await db.close()
