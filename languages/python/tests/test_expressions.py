"""JSON paths, containment and merge, on a schema loaded at runtime."""

import os

import pytest

import monorm
from monorm import QueryError, func

SOURCE = """
model Doc {
  id    BigInt  @id @default(autoincrement())
  title String
  meta  Json
  tags  String[] @default([])
  @@map("expr_docs")
}
"""
URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


def load(source: str = SOURCE):
    registry = monorm.Registry()
    return registry, monorm.loads(source, registry=registry)["Doc"]


@pytest.fixture
async def docs():
    registry, Doc = load()
    try:
        db = await monorm.connect(URL, registry=registry, default=False, max_connections=2)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    objects = Doc.objects.using(db)
    await objects.insert_many(
        [
            {"title": "a", "meta": {"kind": "post", "n": 5, "author": {"name": "Ann"}, "tags": ["x", "y"]}},
            {"title": "b", "meta": {"kind": "page", "n": 1, "author": {"name": "Bob"}, "tags": ["y"]}},
            {"title": "c", "meta": {"kind": "post", "n": 9}},
        ]
    )
    yield Doc, objects
    await db.drop_tables()
    await db.close()


def test_json_path_sql():
    _, Doc = load()
    assert (
        Doc.objects.filter(Doc.meta["author"]["name"] == "Ann")
        .sql()
        .endswith("""WHERE ((("expr_docs"."meta" -> E'author') -> E'name')) = '"Ann"'""")
    )
    assert (
        Doc.objects.filter(Doc.meta["tags"][0].as_text() == "x")
        .sql()
        .endswith("""WHERE ((("expr_docs"."meta" -> E'tags') ->> 0)) = 'x'""")
    )
    assert (
        Doc.objects.filter(Doc.meta.json_contains({"kind": "post"}))
        .sql()
        .endswith("""WHERE "expr_docs"."meta" @> '{"kind":"post"}'""")
    )
    assert Doc.objects.filter(Doc.meta.has_key("tags")).sql().endswith("""WHERE "expr_docs"."meta" ? 'tags'""")
    assert (
        Doc.objects.filter(Doc.meta.has_keys(["a", "b"]))
        .sql()
        .endswith("""WHERE ("expr_docs"."meta" ? 'a') AND ("expr_docs"."meta" ? 'b')""")
    )
    assert (
        Doc.objects.filter(Doc.meta.has_any_keys(["a", "b"]))
        .sql()
        .endswith("""WHERE ("expr_docs"."meta" ? 'a') OR ("expr_docs"."meta" ? 'b')""")
    )
    # On an array column, an int index stays SQL's array element access.
    assert '("expr_docs"."tags")[1]' in Doc.objects.select(Doc.tags[1]).sql()
    with pytest.raises(TypeError, match="not a JSON column"):
        Doc.title["a"]  # type: ignore[index]
    with pytest.raises(TypeError, match="ends a JSON path"):
        Doc.meta["a"].as_text()["b"]  # type: ignore[index]


async def test_json_paths(docs):
    Doc, objects = docs
    assert await objects.filter(Doc.meta["author"]["name"] == "Ann").select(Doc.title).scalars() == ["a"]
    assert await objects.filter(Doc.meta["n"] > 3).order_by(Doc.id).select(Doc.title).scalars() == ["a", "c"]
    assert await objects.filter(Doc.meta["tags"][0].as_text() == "y").select(Doc.title).scalars() == ["b"]
    assert await objects.filter(Doc.meta["author"]["name"].as_text().startswith("B")).select(Doc.title).scalars() == [
        "b"
    ]
    rows = await objects.order_by(Doc.id).select(
        Doc.meta["author"].label("author"), Doc.meta["kind"].as_text().label("kind")
    )
    assert [tuple(r) for r in rows] == [({"name": "Ann"}, "post"), ({"name": "Bob"}, "page"), (None, "post")]
    assert await objects.order_by(Doc.meta["n"].desc()).select(Doc.title).scalars() == ["c", "a", "b"]


async def test_json_containment_and_keys(docs):
    Doc, objects = docs
    assert await objects.filter(Doc.meta.json_contains({"kind": "post"})).count() == 2
    assert await objects.filter(Doc.meta["tags"].json_contains(["y"])).count() == 2
    assert await objects.filter(Doc.meta.json_contained_by({"kind": "post", "n": 9, "x": 1})).select(
        Doc.title
    ).scalars() == ["c"]
    assert await objects.filter(Doc.meta.has_key("author")).count() == 2
    assert await objects.filter(~Doc.meta.has_key("tags")).select(Doc.title).scalars() == ["c"]


async def test_json_merge_in_an_update(docs):
    Doc, objects = docs
    await objects.filter(Doc.title == "c").update(meta=Doc.meta.json_merge({"n": 10, "seen": True}))
    doc = await objects.get(Doc.title == "c")
    assert doc.meta == {"kind": "post", "n": 10, "seen": True}


async def test_json_merge_with_a_path_on_the_right_keeps_the_object(docs):
    Doc, objects = docs
    await objects.insert_many([{"title": "d", "meta": {"a": {"x": 1}, "b": 2, "k": "v"}}])
    await objects.filter(Doc.title == "d").update(meta=Doc.meta.json_merge(Doc.meta["a"]))
    assert (await objects.get(Doc.title == "d")).meta == {"a": {"x": 1}, "b": 2, "k": "v", "x": 1}
    assert await objects.filter(Doc.title == "d").select(Doc.title.concat(Doc.meta["k"].as_text())).scalars() == ["dv"]


async def test_json_merge_with_a_column(docs):
    Doc, objects = docs
    await objects.filter(Doc.title == "c").update(meta=Doc.meta.json_merge(Doc.meta))
    assert (await objects.get(Doc.title == "c")).meta == {"kind": "post", "n": 9}


async def test_json_methods_refuse_null(docs):
    Doc, objects = docs
    for call in (Doc.meta.json_merge, Doc.meta.json_contains, Doc.meta.json_contained_by):
        with pytest.raises(TypeError, match="not None"):
            call(None)
    assert (await objects.get(Doc.title == "c")).meta == {"kind": "post", "n": 9}


async def test_json_containment_takes_a_param(docs):
    Doc, objects = docs
    contains = objects.filter(Doc.meta.json_contains(monorm.param("v"))).order_by(Doc.title).prepare()
    assert [d.title for d in await contains(v={"kind": "post"})] == ["a", "c"]
    contained = objects.filter(Doc.meta.json_contained_by(monorm.param("v"))).prepare()
    assert [
        d.title for d in await contained(v={"kind": "page", "n": 1, "author": {"name": "Bob"}, "tags": ["y"], "z": 0})
    ] == ["b"]
    with pytest.raises(TypeError, match="not the expression"):
        Doc.meta.json_contains(Doc.meta["a"])


def test_a_json_index_outside_the_32_bit_range_is_an_error(docs):
    Doc, objects = docs
    assert "-> 2147483647" in objects.select(Doc.meta["arr"][2**31 - 1]).sql()
    for index in (2**31, 2**62, -(2**31) - 1, -(2**63)):
        with pytest.raises(QueryError, match="JSON index"):
            objects.select(Doc.meta["arr"][index]).sql()


KEYS_SOURCE = """
model Doc {
  id    BigInt  @id @default(autoincrement())
  title String
  meta  Json?
  @@map("expr_key_docs")
}
"""
KEYS_DOCS = [
    ("a", {"kind": "post", "n": 5, "tags": ["x", "y"], "author": {"name": "Ann"}}),
    ("b", {"kind": "page", "n": 1, "tags": ["y"]}),
    ("c", ["a", "b"]),
    ("d", "a"),
    ("e", 5),
    ("f", None),
    ("g", {"a": None}),
    ("h", [{"a": 1, "b": 2}, {"c": 3}]),
    ("i", [1, [2, 3]]),
]


@pytest.fixture(params=["postgres", "sqlite"])
async def keys(request):
    if request.param == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + KEYS_SOURCE
        url = "sqlite://:memory:"
    else:
        source, url = KEYS_SOURCE, URL
    registry, Doc = load(source)
    try:
        db = await monorm.connect(url, registry=registry, default=False, max_connections=2)
    except monorm.DatabaseError as e:
        pytest.skip(f"database not reachable at {url}: {e}")
    await db.drop_tables()
    await db.create_tables()
    objects = Doc.objects.using(db)
    await objects.insert_many([{"title": title, "meta": meta} for title, meta in KEYS_DOCS])
    yield Doc, objects
    await db.drop_tables()
    await db.close()


async def test_json_keys_and_containment_give_the_same_rows_on_both_databases(keys):
    Doc, objects = keys
    meta = Doc.meta

    async def titles(cond):
        return await objects.filter(cond).order_by(Doc.id).select(Doc.title).scalars()

    # `?`: an object matches a key, an array or a string matches an equal string.
    assert await titles(meta.has_key("kind")) == ["a", "b"]
    assert await titles(meta.has_key("a")) == ["c", "d", "g"]
    assert await titles(meta.has_key("1")) == []
    # A NULL column is neither a match nor a non-match.
    assert await titles(~meta.has_key("kind").or_null()) == ["c", "d", "e", "g", "h", "i"]
    assert await titles(~meta.has_key("kind").and_not_null()) == ["c", "d", "e", "f", "g", "h", "i"]
    assert await titles(meta.has_keys(["kind", "n"])) == ["a", "b"]
    assert await titles(meta.has_keys(["kind", "author"])) == ["a"]
    assert await titles(meta.has_keys(["a"])) == ["c", "d", "g"]
    assert await titles(meta.has_any_keys(["author", "n"])) == ["a", "b"]
    assert await titles(meta.has_any_keys(["a", "zzz"])) == ["c", "d", "g"]
    assert await titles(meta.has_any_keys([])) == []
    assert len(await titles(meta.has_keys([]))) == len(KEYS_DOCS)
    # `@>`.
    assert await titles(meta.json_contains({"kind": "post"})) == ["a"]
    assert await titles(meta.json_contains({"author": {"name": "Ann"}})) == ["a"]
    assert await titles(meta.json_contains({"tags": ["y"]})) == ["a", "b"]
    assert await titles(meta.json_contains({"tags": ["y", "y"]})) == ["a", "b"]
    assert await titles(meta.json_contains({"n": 5.0})) == ["a"]
    assert await titles(meta.json_contains({"n": "5"})) == []
    assert await titles(meta.json_contains(["a"])) == ["c"]
    assert await titles(meta.json_contains("a")) == ["c", "d"]
    assert await titles(meta.json_contains(["b", "a", "a"])) == ["c"]
    assert await titles(meta.json_contains(5)) == ["e"]
    assert await titles(meta.json_contains({"a": None})) == ["g"]
    assert await titles(meta.json_contains([{"a": 1}, {"c": 3}])) == ["h"]
    assert await titles(meta.json_contains([{"a": 1, "c": 3}])) == []
    assert await titles(meta.json_contains([[2]])) == ["i"]
    assert await titles(meta.json_contains([2])) == []
    assert await titles(meta.json_contains({})) == ["a", "b", "g"]
    assert await titles(meta.json_contains([])) == ["c", "h", "i"]
    # `<@`.
    assert await titles(meta.json_contained_by({"kind": "page", "n": 1, "tags": ["y", "z"], "x": 0})) == ["b"]
    assert await titles(meta.json_contained_by(["a", "b", "c"])) == ["c", "d"]
    assert await titles(meta.json_contained_by({"a": None, "b": 1})) == ["g"]
    assert await titles(~meta.json_contains({"kind": "post"}).or_null()) == ["b", "c", "d", "e", "g", "h", "i"]
    assert await titles(~meta.json_contains({"kind": "post"}).and_not_null()) == ["b", "c", "d", "e", "f", "g", "h", "i"]


async def test_json_keys_and_containment_take_a_param(keys):
    Doc, objects = keys
    has = objects.filter(Doc.meta.has_key("kind")).order_by(Doc.id).prepare()
    assert [d.title for d in await has()] == ["a", "b"]
    contains = objects.filter(Doc.meta.json_contains(monorm.param("v"))).order_by(Doc.id).prepare()
    assert [d.title for d in await contains(v={"kind": "post"})] == ["a"]
    assert [d.title for d in await contains(v="a")] == ["c", "d"]
    assert [d.title for d in await contains(v=["a"])] == ["c"]
    assert [d.title for d in await contains(v=5)] == ["e"]
    contained = objects.filter(Doc.meta.json_contained_by(monorm.param("v"))).order_by(Doc.id).prepare()
    assert [d.title for d in await contained(v=["a", "b"])] == ["c", "d"]


def test_json_key_lists_take_a_list_of_strings():
    _, Doc = load()
    for call in (Doc.meta.has_keys, Doc.meta.has_any_keys):
        with pytest.raises(TypeError, match="list"):
            call("kind")


async def test_json_operators_on_sqlite_are_an_error():
    registry, Doc = load(
        'datasource db { provider = "sqlite" }\n' + SOURCE.replace("  tags  String[] @default([])\n", "")
    )
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        # A path, a merge and a path as the left side of a containment test stay PostgreSQL only.
        for cond in (Doc.meta["a"] == 1, Doc.meta["a"].json_contains({"a": 1}), Doc.meta["a"].has_key("a")):
            with pytest.raises(QueryError, match="sqlite does not support"):
                await Doc.objects.using(db).filter(cond).count()
        with pytest.raises(QueryError, match="sqlite does not support"):
            await Doc.objects.using(db).filter(Doc.title == "x").update(meta=Doc.meta.json_merge({"a": 1}))
    finally:
        await db.close()


# -- full-text search -------------------------------------------------------------------------

ARTICLES = """
model Article {
  id   BigInt @id @default(autoincrement())
  body String
  @@index([sql("to_tsvector('english', body)")], type: Gin, name: "articles_body_search")
  @@map("expr_articles")
}
"""


def load_articles(source: str = ARTICLES):
    registry = monorm.Registry()
    return registry, monorm.loads(source, registry=registry)["Article"]


def test_full_text_search_sql():
    from monorm import func

    registry, Article = load_articles()
    vector = func.to_tsvector("english", Article.body)
    assert (
        Article.objects.filter(vector.matches("running dogs"))
        .sql()
        .endswith(
            """WHERE TO_TSVECTOR('english'::regconfig, "expr_articles"."body") @@ PLAINTO_TSQUERY('english'::regconfig, 'running dogs')"""
        )
    )
    sql = Article.objects.order_by(func.ts_rank(vector, func.websearch_to_tsquery("english", "fox -lazy")).desc()).sql()
    assert (
        "ORDER BY CAST(TS_RANK(TO_TSVECTOR('english'::regconfig, " in sql
        and "WEBSEARCH_TO_TSQUERY('english'::regconfig, 'fox -lazy')" in sql
    )
    assert (
        'TO_TSVECTOR("expr_articles"."body") @@ TO_TSQUERY(\'cat & !dog\')'
        in Article.objects.filter(func.to_tsvector(Article.body).matches(func.to_tsquery("cat & !dog"))).sql()
    )
    # The GIN index on the same expression, from the schema.
    assert any(
        'CREATE INDEX IF NOT EXISTS "articles_body_search" ON "expr_articles" USING gin ((to_tsvector(\'english\', body)))'
        in s
        for s in registry.native().ddl()
    ), registry.native().ddl()
    with pytest.raises(QueryError, match="text search configuration"):
        Article.objects.filter(func.to_tsvector("english'; drop", Article.body).matches("x")).sql()


def test_full_text_search_config_names():
    from monorm import func

    registry, Article = load_articles()
    assert (
        "'español'::regconfig" in Article.objects.filter(func.to_tsvector("español", Article.body).matches("x")).sql()
    )
    for bad in ("a'b", "a b", "a.b.c", "1a", ""):
        with pytest.raises(QueryError, match="text search configuration"):
            Article.objects.filter(func.to_tsvector(bad, Article.body).matches("x")).sql()


async def test_matches_prepared_param():
    from monorm import func

    registry, Article = load_articles()
    try:
        db = await monorm.connect(URL, registry=registry, default=False, max_connections=2)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        objects = Article.objects.using(db)
        await objects.insert_many(
            [{"body": "The quick brown fox"}, {"body": "A lazy dog sleeps"}, {"body": "Dogs are running"}]
        )
        vector = func.to_tsvector("english", Article.body)
        found = objects.filter(vector.matches(monorm.param("q"))).order_by(Article.id).prepare()
        assert [a.id for a in await found(q="dogs")] == [2, 3]
        assert [a.id for a in await found(q="fox")] == [1]
    finally:
        await db.drop_tables()
        await db.close()


async def test_matches_forms():
    from monorm import func

    registry, Article = load_articles()
    try:
        db = await monorm.connect(URL, registry=registry, default=False, max_connections=2)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        objects = Article.objects.using(db)
        await objects.insert_many(
            [{"body": "The quick brown fox"}, {"body": "A lazy dog sleeps"}, {"body": "Dogs are running"}]
        )
        vector = func.to_tsvector("english", Article.body)
        both = func.plainto_tsquery("english", "lazy dog")
        assert await objects.filter(vector.matches(both)).select(Article.id).scalars() == [2]
        # a vector that is not to_tsvector(config, ...) is searched with the server's default config
        wrapped = func.coalesce(vector, func.to_tsvector("english", ""))
        assert await objects.filter(wrapped.matches("fox")).select(Article.id).scalars() == [1]
    finally:
        await db.drop_tables()
        await db.close()


async def test_full_text_search():
    from monorm import func

    registry, Article = load_articles()
    try:
        db = await monorm.connect(URL, registry=registry, default=False, max_connections=2)
    except monorm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        objects = Article.objects.using(db)
        await objects.insert_many(
            [
                {"body": "The quick brown fox jumps"},
                {"body": "A lazy dog sleeps"},
                {"body": "Dogs are running; the fox and the dog"},
            ]
        )
        vector = func.to_tsvector("english", Article.body)
        found = await objects.filter(vector.matches("running dogs")).select(Article.id).scalars()
        assert found == [3]  # stemmed: "running" is "run", "dogs" is "dog"
        query = func.websearch_to_tsquery("english", "fox -lazy")
        ranked = (
            await objects.filter(vector.matches(query))
            .order_by(func.ts_rank(vector, query).desc(), Article.id)
            .select(Article.id)
            .scalars()
        )
        assert sorted(ranked) == [1, 3]
        rank = (
            await objects.filter(Article.id == 3)
            .select(func.ts_rank(vector, func.to_tsquery("english", "dog")))
            .scalar()
        )
        assert isinstance(rank, float) and rank > 0
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.case("sqlite::full_text_search_on_sqlite_is_an_error")
async def test_full_text_search_on_sqlite_is_an_error():
    from monorm import func

    registry, Article = load_articles(
        'datasource db { provider = "sqlite" }\n'
        + ARTICLES.replace(
            '  @@index([sql("to_tsvector(\'english\', body)")], type: Gin, name: "articles_body_search")\n', ""
        )
    )
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(QueryError, match="sqlite does not support full-text search"):
            await Article.objects.using(db).filter(func.to_tsvector(Article.body).matches("x")).count()
    finally:
        await db.close()


async def test_case_of_plain_strings_takes_the_enum_type_of_the_column_it_is_assigned_to():
    source = (
        "enum Level {\n  basic\n  admin\n}\n"
        "model Member {\n  id BigInt @id @default(autoincrement())\n  n Int\n  level Level @default(basic)\n  @@map(\"expr_members_py\")\n}"
    )
    registry = monorm.Registry()
    Member = monorm.loads(source, registry=registry)["Member"]
    pg = await monorm.connect(URL, default=False, registry=registry, max_connections=1)
    try:
        await pg.drop_tables()
        await pg.create_tables()
        members = Member.objects.using(pg)
        await members.insert_many([{"n": 1}, {"n": 9}])
        await members.all().update(level=func.case((Member.n > 5, "admin"), default="basic"))
        assert await members.order_by(Member.id).select(Member.level).scalars() == ["basic", "admin"]
        await members.all().update(level="basic")
        assert await members.filter(Member.level == func.case((Member.n > 5, "admin"), default="basic")).count() == 1
    finally:
        await pg.drop_tables()
        await pg.close()
