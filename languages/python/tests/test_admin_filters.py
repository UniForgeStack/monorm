"""Admin lists: filters, computed and aggregate columns, and to-many tabs, on PostgreSQL and SQLite."""

import os
from datetime import datetime, timezone
from types import SimpleNamespace

import pytest

import monorm
from monorm import admin

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

SOURCE = """
datasource db {
  provider = "%(provider)s"
}

enum FltLevel {
  low
  high

  @@storage(text)
}

model FltAuthor {
  id       Int          @id
  name     String
  active   Boolean
  nick     String?
  comments FltComment[]
  @@map("flt_authors")
}

model FltComment {
  id        Int        @id
  body      String
  author_id Int?
  level     FltLevel
  rank      Int
  at        DateTime
  author    FltAuthor? @relation(fields: [author_id], references: [id])
  @@map("flt_comments")
}
"""

SU = {"is_superuser": True}
COMMENTS = "/admin/api/resources/FltComment"
AUTHORS = "/admin/api/resources/FltAuthor"


def bodies(response):
    return [row[0] for row in response.json()["rows"]]


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request):
    registry = monorm.Registry()
    models = monorm.loads(SOURCE % {"provider": request.param}, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    Author, Comment = models["FltAuthor"], models["FltComment"]
    await Author.objects.using(db).insert_many(
        [
            {"id": 1, "name": "Ada", "active": True, "nick": None},
            {"id": 2, "name": "Bo", "active": False, "nick": ""},
            {"id": 3, "name": "Cy", "active": True, "nick": "cy"},
        ]
    )
    day = lambda *a: datetime(*a, tzinfo=timezone.utc)  # noqa: E731
    await Comment.objects.using(db).insert_many(
        [
            {"id": 1, "body": "one", "author_id": 1, "level": "high", "rank": 1, "at": day(2026, 1, 2, 3, 4, 5)},
            {"id": 2, "body": "two", "author_id": None, "level": "low", "rank": 1, "at": day(2026, 2, 1)},
            {"id": 3, "body": "three", "author_id": 2, "level": "high", "rank": 2, "at": day(2026, 3, 1)},
            {"id": 4, "body": "four", "author_id": 1, "level": "low", "rank": 3, "at": day(2026, 3, 15, 12)},
        ]
    )
    yield SimpleNamespace(provider=request.param, registry=registry, db=db, Author=Author, Comment=Comment)
    await db.drop_tables()
    await db.close()


def site_of(w, comment=None, author=None, **options):
    site = admin.Admin(w.registry, **options)
    C, A = w.Comment, w.Author
    comment_list = {
        "columns": [C.body],
        "order": [C.id],
        "filters": [C.level, C.rank, C.at, C.author, C.author.name, C.author.active],
    }
    comment_list.update(comment or {})
    author_list = {"columns": [A.name, A.active, A.nick], "order": [A.id], "filters": [A.active, A.nick, A.name]}
    author_list.update(author or {})
    site.register(C, {"list": comment_list})
    site.register(A, {"list": author_list})
    return site, site.test_client(w.db, user=SU)


async def test_an_enum_a_bool_and_a_field_of_a_related_model_filter_and_several_values_mean_or(world):
    _, client = site_of(world)
    ask = lambda q: client.get(f"{COMMENTS}?{q}")  # noqa: E731
    assert bodies(await ask("f.level=high")) == ["one", "three"]
    assert bodies(await ask("f.level=high&f.level=low")) == ["one", "two", "three", "four"]
    assert bodies(await ask("f.author.active=true")) == ["one", "four"]
    assert bodies(await ask("f.author.name=Bo")) == ["three"]
    assert bodies(await ask("f.level=high&f.author.name=Ada")) == ["one"]
    related = await ask("f.author.name=Bo")
    assert len(related.sql) == len((await client.get("/admin/api/resources")).sql) + 1
    assert [row[0] for row in (await client.get(f"{AUTHORS}?f.active=false")).json()["rows"]] == ["Bo"]


async def test_a_range_and_a_relation_filter_and_the_empty_operators(world):
    _, client = site_of(world)
    ask = lambda q: client.get(f"{COMMENTS}?{q}")  # noqa: E731
    assert bodies(await ask("f.rank:gte=2")) == ["three", "four"]
    assert bodies(await ask("f.rank:gt=1&f.rank:lt=3")) == ["three"]
    assert bodies(await ask("f.at:gte=2026-03-01&f.at:lt=2026-03-02")) == ["three"]
    assert bodies(await ask("f.at:gte=2026-03-15T12:00:00Z")) == ["four"]
    assert bodies(await ask("f.author=1")) == ["one", "four"]
    assert bodies(await ask("f.author=1&f.author=2")) == ["one", "three", "four"]
    assert bodies(await ask("f.author:empty")) == ["two"]
    assert bodies(await ask("f.author:notempty")) == ["one", "three", "four"]
    assert [r[0] for r in (await client.get(f"{AUTHORS}?f.nick:empty")).json()["rows"]] == ["Ada", "Bo"]
    assert [r[0] for r in (await client.get(f"{AUTHORS}?f.nick:notempty")).json()["rows"]] == ["Cy"]


async def test_a_wrong_value_is_422_and_an_unknown_filter_is_400(world):
    _, client = site_of(world)
    for q in ["f.level=middle", "f.rank=x", "f.rank:gte=1&f.rank:gte=2", "f.at=yesterday", "f.author=x", "f.level:gt=1"]:
        response = await client.get(f"{COMMENTS}?{q}")
        assert response.status == 422, q
        assert response.json()["error"]["kind"] == "Invalid", q
    for q in ["f.body=one", "n.nothing=x", "f.rank:between=1"]:
        assert (await client.get(f"{COMMENTS}?{q}")).status == 400, q


async def test_an_unknown_or_unfit_filter_field_is_a_build_error(world):
    C = world.Comment
    for filters, text in [(["nope"], "nope"), ([world.Author.comments], "comments")]:
        site = admin.Admin(world.registry)
        site.register(C, {"list": {"filters": filters}})
        site.register(world.Author)
        with pytest.raises(admin.AdminConfigError, match=text):
            site.check()
    site = admin.Admin(world.registry)
    site.register(C, {"list": {"filters": [C.author.nope if False else "author.nope"]}})
    site.register(world.Author)
    with pytest.raises(admin.AdminConfigError, match="author.nope"):
        site.check()


async def test_a_named_filter_has_a_default_and_an_all_choice(world):
    C = world.Comment
    named = [
        {
            "name": "recent",
            "label": "Age",
            "default": "march",
            "choices": [
                {"name": "march", "label": "Rank two or more", "condition": C.rank >= 2},
                {"name": "first", "label": "Rank one", "condition": C.rank == 1},
            ],
        }
    ]
    _, client = site_of(world, comment={"named_filters": named})
    assert bodies(await client.get(COMMENTS)) == ["three", "four"]
    assert bodies(await client.get(f"{COMMENTS}?n.recent=first")) == ["one", "two"]
    assert bodies(await client.get(f"{COMMENTS}?n.recent=all")) == ["one", "two", "three", "four"]
    assert (await client.get(f"{COMMENTS}?n.recent=nope")).status == 422
    resource = next(r for r in (await client.get("/admin/api/resources")).json()["resources"] if r["name"] == "FltComment")
    assert resource["named_filters"][0]["default"] == "march"
    assert resource["named_filters"][0]["choices"][1] == {"name": "first", "title": "Rank one"}
    assert (await client.get("/admin/api/counts/FltComment")).json()["count"] == 2


async def test_a_named_filter_with_a_parameter_is_a_build_error(world):
    C = world.Comment
    site = admin.Admin(world.registry)
    named = [{"name": "n", "label": "N", "choices": [{"name": "c", "label": "C", "condition": C.rank == monorm.param("r")}]}]
    with pytest.raises(admin.AdminConfigError, match="placeholder"):
        site.register(C, {"list": {"named_filters": named}})
        site.check()


async def test_an_aggregate_over_a_relation_is_one_statement(world):
    A = world.Author
    aggregates = [
        {"name": "comment_count", "label": "Comments", "relation": A.comments, "fn": "count"},
        {"name": "top_rank", "label": "Top rank", "relation": A.comments, "fn": "max", "field": A.comments.rank},
    ]
    _, client = site_of(world, author={"columns": [A.name, "comment_count", "top_rank"], "aggregates": aggregates})
    preamble = len((await client.get("/admin/api/resources")).sql)
    page = await client.get(AUTHORS)
    assert page.status == 200, page.json()
    assert len(page.sql) == preamble + 1, "no N+1"
    rows = page.json()["rows"]
    assert [int(r[1]) for r in rows] == [2, 1, 0]
    assert [r[2] for r in rows] == [3, 2, None]
    assert page.json()["columns"][1]["title"] == "Comments"


async def test_a_computed_expression_column_is_in_the_query_and_sorts_when_the_config_allows(world):
    C = world.Comment
    computed = [{"name": "body_length", "type": "int", "nullable": False, "sortable": True, "expr": monorm.func.length(C.body)}]
    _, client = site_of(world, comment={"columns": [C.body, "body_length"], "computed": computed})
    page = await client.get(f"{COMMENTS}?sort=body_length")
    assert bodies(page) == ["one", "two", "four", "three"]
    assert [r[1] for r in page.json()["rows"]] == [3, 3, 4, 5]
    assert bodies(await client.get(f"{COMMENTS}?sort=-body_length")) == ["three", "four", "one", "two"]
    plain = [{"name": "body_length", "type": "int", "expr": monorm.func.length(C.body)}]
    _, client = site_of(world, comment={"columns": [C.body, "body_length"], "computed": plain})
    assert (await client.get(f"{COMMENTS}?sort=body_length")).status == 400


async def test_a_computed_column_with_the_wrong_type_or_a_name_clash_is_a_build_error(world):
    C = world.Comment
    length = monorm.func.length(C.body)

    def build(column):
        site = admin.Admin(world.registry)
        site.register(C, {"list": {"computed": [column]}})
        site.register(world.Author)
        site.check()

    with pytest.raises(admin.AdminConfigError, match="declared"):
        build({"name": "n", "type": "text", "expr": length})
    with pytest.raises(admin.AdminConfigError, match="name of a field"):
        build({"name": "body", "type": "int", "expr": length})
    with pytest.raises(admin.AdminConfigError, match="expr or function"):
        build({"name": "n", "type": "int"})


async def test_a_batch_function_column_runs_once_for_the_page(world):
    C = world.Comment
    calls = []

    async def doubled(keys):
        calls.append(keys)
        return [key * 2 for key in keys]

    computed = [{"name": "doubled", "type": "int", "nullable": False, "function": doubled}]
    _, client = site_of(world, comment={"columns": [C.body, "doubled"], "computed": computed, "page_size": 3})
    page = await client.get(COMMENTS)
    assert page.status == 200, page.json()
    assert calls == [[1, 2, 3]], "one call for three rows"
    assert [r[1] for r in page.json()["rows"]] == [2, 4, 6]

    async def wrong(keys):
        return ["x"] * len(keys)

    computed = [{"name": "doubled", "type": "int", "nullable": False, "function": wrong}]
    _, client = site_of(world, comment={"columns": [C.body, "doubled"], "computed": computed})
    assert (await client.get(COMMENTS)).status == 500, "a value of the wrong type is a server error"
    assert (await client.get(f"{COMMENTS}?sort=doubled")).status == 400


async def test_a_to_many_tab_lists_the_children_of_one_record_with_the_child_list_features(world):
    C = world.Comment
    _, client = site_of(world, comment={"search": [C.body], "page_size": 10})
    tab = "/admin/api/resources/FltAuthor/1/comments"
    page = await client.get(tab)
    assert page.status == 200, page.json()
    assert bodies(page) == ["one", "four"]
    assert bodies(await client.get(f"{tab}?f.level=high")) == ["one"]
    assert bodies(await client.get(f"{tab}?q=fou")) == ["four"]
    assert bodies(await client.get(f"{tab}?sort=-id")) == ["four", "one"]
    assert (await client.get("/admin/api/counts/FltAuthor/1/comments")).json()["count"] == 2
    assert (await client.get("/admin/api/resources/FltAuthor/99/comments")).status == 404
    assert (await client.get("/admin/api/resources/FltAuthor/1/nothing")).status == 404
    author = next(r for r in (await client.get("/admin/api/resources")).json()["resources"] if r["name"] == "FltAuthor")
    assert author["tabs"] == [{"name": "comments", "title": "Comments", "target": "FltComment"}]


async def test_labels_and_empty_texts_reach_the_metadata(world):
    C = world.Comment
    columns = [C.body, C.author.name]
    _, client = site_of(world, comment={"columns": columns, "labels": {C.body: "Text"}, "empty": "none", "empty_fields": {C.author.name: "no author"}})
    resource = next(r for r in (await client.get("/admin/api/resources")).json()["resources"] if r["name"] == "FltComment")
    assert resource["columns"][0]["title"] == "Text"
    assert resource["columns"][0]["empty"] == "none"
    assert resource["columns"][1]["empty"] == "no author"
    site = admin.Admin(world.registry)
    site.register(C, {"list": {"labels": {"nope": "x"}}})
    site.register(world.Author)
    with pytest.raises(admin.AdminConfigError, match="nope"):
        site.check()
