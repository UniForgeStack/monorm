"""SQL shape tests: no database needed."""

from datetime import datetime, timezone

import pytest
from blog.models import Comment, Post, Profile, User

import orm
from orm import QueryError, and_, excluded, func, or_

Y = datetime(2026, 10, 1, tzinfo=timezone.utc)
USER_COLS = 'SELECT "users"."id", "users"."email", "users"."name", "users"."created_at" FROM "users"'


def where(qs) -> str:
    sql = qs.sql()
    return sql.split(" WHERE ", 1)[1] if " WHERE " in sql else ""


def test_to_many_filter_is_exists():
    query = User.objects.filter(User.posts.created_at < Y)
    assert query.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 0, 1, 0, 0),
    ))
    sql = query.sql()
    assert sql == (
        f"{USER_COLS} WHERE EXISTS(SELECT 1 FROM \"posts\" AS \"t1\" "
        "WHERE \"t1\".\"author_id\" = \"users\".\"id\" "
        "AND \"t1\".\"created_at\" < '2026-10-01 00:00:00.000000 +00:00')"
    )


def test_same_filter_call_means_same_related_row():
    query = User.objects.filter(User.posts.views > 10, User.posts.published == True)  # noqa: E712
    assert query.inspect().statements[0].correlated_count == 1
    w = where(query)
    assert '"t1"."views" > 10 AND "t1"."published" = TRUE' in w


def test_separate_filter_calls_are_independent():
    result = User.objects.filter(User.posts.views > 10).filter(User.posts.published == True).inspect()  # noqa: E712
    assert result.statements[0].correlated_count == 2


def test_or_inside_one_relation_shares_subquery():
    query = User.objects.filter((User.posts.views > 10) | (User.posts.title == "x"))
    assert query.inspect().statements[0].correlated_count == 1
    assert " OR " in where(query)


def test_or_with_local_column_keeps_rows_without_related():
    query = User.objects.filter(or_(User.email == "a", User.posts.views > 10))
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert w.startswith('"users"."email" = \'a\' OR EXISTS(')


def test_exclude_is_not_exists():
    query = User.objects.exclude(User.posts.published == False)  # noqa: E712
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert w.startswith("NOT EXISTS(")


def test_negation_stays_outside_subquery():
    query = User.objects.filter(and_(User.posts.views > 10, ~(User.posts.published == True)))  # noqa: E712
    assert query.inspect().statements[0].correlated_count == 2
    assert "NOT EXISTS" in where(query)


def test_nested_relation_nests_subqueries():
    query = User.objects.filter(User.posts.comments.body.contains("50%"))
    assert query.inspect().statements[0].correlated_count == 2
    w = where(query)
    assert '"t2"."post_id" = "t1"."id"' in w
    assert "LIKE E'%50\\\\%%'" in w  # escaped % (debug rendering inlines values)


def test_to_one_filter():
    query = Post.objects.filter(Post.author.email == "a@b.c")
    assert query.inspect().statements[0].joins_count == 0
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert w == 'EXISTS(SELECT 1 FROM "users" AS "t1" WHERE "t1"."id" = "posts"."author_id" AND "t1"."email" = \'a@b.c\')'


def test_column_to_column_across_relation():
    query = Post.objects.filter(Post.comments.created_at < Post.created_at)
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert '"t1"."created_at" < "posts"."created_at"' in w


def test_to_one_load_and_order_use_left_joins():
    query = Comment.objects.load(Comment.post.author).order_by(Comment.post.title.desc())
    assert query.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 2, 0, 0, 0),
    ))  # ordering reuses the load's join
    sql = query.sql()
    assert 'LEFT JOIN "posts" AS "j1" ON "j1"."id" = "comments"."post_id"' in sql
    assert 'LEFT JOIN "users" AS "j2" ON "j2"."id" = "j1"."author_id"' in sql
    assert sql.endswith('ORDER BY "j1"."title" DESC')


def test_slicing():
    assert Post.objects.all()[5:10].sql().endswith("LIMIT 5 OFFSET 5")
    assert Post.objects.all()[5:10][1:3].sql().endswith("LIMIT 2 OFFSET 6")


def test_null_comparisons():
    assert where(Comment.objects.filter(Comment.author_id == None)) == '"comments"."author_id" IS NULL'  # noqa: E711
    assert where(Comment.objects.filter(Comment.author_id != None)) == '"comments"."author_id" IS NOT NULL'  # noqa: E711


def test_in_and_empty_in():
    assert where(User.objects.filter(User.id.in_([1, 2]))) == '"users"."id" IN (1, 2)'
    assert where(User.objects.filter(User.id.in_([]))) == "FALSE"


def test_order_by_to_many_is_rejected():
    with pytest.raises(QueryError, match="to-one"):
        User.objects.order_by(User.posts.views).sql()


def test_column_from_other_model_is_rejected():
    with pytest.raises(ValueError, match="belongs to User"):
        Post.objects.filter(User.email == "x").sql()


def test_python_boolean_operators_are_rejected():
    with pytest.raises(TypeError, match="& | ~"):
        (User.email == "a") and (User.name == "b")  # noqa: B015


def test_unknown_relation_attribute():
    with pytest.raises(AttributeError, match="no field or relation 'nope'"):
        User.posts.nope  # noqa: B018


def test_lock():
    assert User.objects.lock().sql() == f'{USER_COLS} FOR UPDATE OF "users"'
    assert User.objects.lock(exclusive=False).sql().endswith('FOR SHARE OF "users"')
    assert User.objects.lock(nowait=True).sql().endswith('FOR UPDATE OF "users" NOWAIT')
    assert User.objects.lock(False, skip_locked=True).sql().endswith('FOR SHARE OF "users" SKIP LOCKED')
    # Only the model's rows: rows joined by load() stay unlocked.
    sql = Post.objects.load(Post.author).filter(Post.views > 1)[:5].lock().sql()
    assert sql.endswith('LIMIT 5 FOR UPDATE OF "posts"')
    with pytest.raises(ValueError):
        User.objects.lock(nowait=True, skip_locked=True)


def test_lock_refuses_a_filtered_aggregate():
    with pytest.raises(QueryError, match="aggregates"):
        Post.objects.lock().select(func.sum(Post.views, filter=Post.author.name == "A")).sql()


def test_lock_rejected_where_meaningless():
    with pytest.raises(QueryError):
        User.objects.lock().update(name="x")
    with pytest.raises(QueryError):
        User.objects.lock().delete()


def test_writes_validate_when_built():
    with pytest.raises(TypeError):
        User.objects.update(nope=1)
    with pytest.raises(QueryError):
        User.objects.all()[:3].delete()
    with pytest.raises(TypeError):
        excluded(User.posts.views)
    with pytest.raises(TypeError):
        User.objects.insert(email="a", name="b").on_conflict(User.email, update=True, update_fields=[User.name], update_values={"name": "x"})


def native():
    return Post._meta.registry.native()


def test_update_many_joins_a_values_list():
    (sql,) = native().update_many_sql("Post", ["id", "title", "views"], [[1, "a", 10], [2, "b", 20]])
    assert sql == (
        'UPDATE "posts" SET "title" = "v"."column2", "views" = "v"."column3" '
        "FROM (VALUES (1, 'a', 10), (2, 'b', 20)) AS \"v\" WHERE \"posts\".\"id\" = \"v\".\"column1\""
    )


def test_update_many_falls_back_to_case():
    (sql,) = native().update_many_sql("Post", ["id", "views"], [[1, 10], [2, 20]], disable=["update_from_values"])
    assert sql == (
        'UPDATE "posts" SET "views" = (CASE WHEN ("posts"."id" = 1) THEN 10 '
        'WHEN ("posts"."id" = 2) THEN 20 END) WHERE "posts"."id" IN (1, 2)'
    )


def test_update_many_batches():
    rows = [{"id": i, "title": "x"} for i in range(5)]
    result = Post.objects.update_many(rows, batch_size=2).inspect()
    assert result.statement_count == 3
    assert result.has_batching and all(s.has_batching for s in result.statements)
    # Without batch_size, as many rows as Postgres' 65535 parameters allow.
    rows = [{"id": i, "views": i} for i in range(40_000)]
    result = Post.objects.update_many(rows).inspect()
    assert result.statement_count == 2
    assert result.has_batching and all(s.has_batching for s in result.statements)


def test_update_many_validation():
    with pytest.raises(ValueError, match="has no id"):
        Post.objects.update_many([{"title": "a"}])
    with pytest.raises(ValueError, match="appears twice"):
        Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 1, "title": "b"}])
    with pytest.raises(ValueError, match="same fields"):
        Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "views": 3}])
    with pytest.raises(ValueError, match="besides id"):
        Post.objects.update_many([{"id": 1}])
    with pytest.raises(TypeError, match="plain values"):
        Post.objects.update_many([{"id": 1, "views": Post.views + 1}])
    with pytest.raises(TypeError, match="no field"):
        Post.objects.update_many([{"id": 1, "nope": 1}])
    with pytest.raises(QueryError):
        Post.objects.all()[:2].update_many([{"id": 1, "views": 1}])
    with pytest.raises(ValueError):
        Post.objects.update_many([{"id": 1, "views": 1}], batch_size=0)


def test_select_group_by_having():
    sql = (
        Post.objects.filter(Post.published)
        .select(Post.author_id, func.count().label("n"), func.sum(Post.views))
        .group_by(Post.author_id)
        .having(func.count() > 2)
        .sql()
    )
    assert sql == (
        'SELECT "posts"."author_id", COUNT(*), CAST(SUM("posts"."views") AS BIGINT) FROM "posts" '
        'WHERE "posts"."published" = TRUE GROUP BY "posts"."author_id" HAVING (COUNT(*)) > 2'
    )


def test_aggregate_over_relation_is_a_correlated_subquery():
    query = User.objects.select(User.id, func.count(User.posts))
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    one = query.sql()
    assert one == (
        'SELECT "users"."id", (SELECT COUNT(*) FROM "posts" AS "a1" WHERE "a1"."author_id" = "users"."id") FROM "users"'
    )
    # Aggregates over different relations stay separate subqueries.
    two = User.objects.select(User.id, func.count(User.posts), func.count(User.comments).label("c")).inspect()
    assert two.statements[0].joins_count == 0
    assert two.statements[0].correlated_count == 2
    # Two hops join inside the subquery; in WHERE it filters per row, no EXISTS / JOIN outside.
    query = User.objects.filter(func.count(User.posts.comments) > 3)
    assert query.inspect().statements[0].joins_count == 1
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert w == (
        '(SELECT COUNT(*) FROM "posts" AS "a1" INNER JOIN "comments" AS "a2" ON "a2"."post_id" = "a1"."id" '
        'WHERE "a1"."author_id" = "users"."id") > 3'
    )


def test_same_relation_aggregates_share_one_lateral_join():
    query = User.objects.select(User.id, func.count(User.posts), func.max(User.posts.views))
    assert query.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 1, 1, 0, 0),
    ))
    sql = query.sql()
    assert sql == (
        'SELECT "users"."id", "l1"."c1", "l1"."c2" FROM "users" LEFT JOIN LATERAL '
        '(SELECT COUNT(*) AS "c1", MAX("a2"."views") AS "c2" FROM "posts" AS "a2" WHERE "a2"."author_id" = "users"."id") '
        'AS "l1" ON TRUE'
    )
    # Per-aggregate options stay inside their FILTER / DISTINCT; the other relation is its own subquery.
    query = User.objects.select(
        func.count(User.posts, filter=User.posts.views > 10).label("popular"),
        func.sum(User.posts.views, distinct=True),
        func.count(User.comments).label("comments"),
    )
    result = query.inspect()
    assert result.statements[0].joins_count == 1
    assert result.statements[0].correlated_count == 2
    sql = query.sql()
    assert 'COUNT(*) FILTER (WHERE "a2"."views" > 10) AS "c1"' in sql and 'SUM(DISTINCT "a2"."views")' in sql
    assert '(SELECT COUNT(*) FROM "comments"' in sql
    # Filter-only and grouped uses keep the correlated subqueries.
    filtered = User.objects.filter(func.count(User.posts) > 1, func.sum(User.posts.views) > 1).select(User.id).inspect()
    assert filtered.statements[0].joins_count == 0
    assert filtered.statements[0].correlated_count == 2
    grouped = User.objects.select(User.id, func.count(User.posts), func.sum(User.posts.views), func.count().label("all")).group_by(User.id).inspect()
    assert grouped.statements[0].joins_count == 0
    assert grouped.statements[0].correlated_count == 2


def test_lateral_join_comes_after_the_to_one_joins():
    sql = Post.objects.select(
        Post.author.name,
        func.count(Post.comments, filter=Post.comments.body == Post.author.name),
        func.max(Post.comments.id),
    ).sql()
    assert 'LEFT JOIN "users" AS "j' in sql and "LEFT JOIN LATERAL" in sql
    assert sql.index('LEFT JOIN "users" AS "j') < sql.index("LEFT JOIN LATERAL")


def test_an_aggregate_with_an_outer_only_filter_is_not_a_lateral_member():
    query = User.objects.order_by(User.id).select(
        User.name, func.count(User.posts, filter=User.name == "Alice"), func.max(User.posts.views)
    )
    assert "LATERAL" not in query.sql()
    assert query.inspect().statements[0].joins_count == 0
    assert query.inspect().statements[0].correlated_count == 2


def _two_posts_aggregates(*extra):
    return (User.id, func.count(User.posts), func.max(User.posts.views), *extra)


def _cte_body():
    cte = User.objects.select(*_two_posts_aggregates()).cte("c")
    return cte.select(cte.c.id)


@pytest.mark.parametrize(
    "build",
    [
        lambda: User.objects.lock().select(*_two_posts_aggregates()),
        lambda: User.objects.select(*_two_posts_aggregates()).having(func.count(User.posts) > 0),
        lambda: User.objects.select(*_two_posts_aggregates()).distinct(),
        lambda: User.objects.select(*_two_posts_aggregates()).distinct(User.id),
        lambda: User.objects.select(*_two_posts_aggregates(func.row_number().over(orm.window(order_by=User.id)))),
        lambda: User.objects.select(*_two_posts_aggregates(func.count().label("n"))),
        _cte_body,
    ],
    ids=["lock", "having", "distinct", "distinct_on", "window", "local_aggregate", "cte"],
)
def test_lateral_guards_keep_scalar_subqueries(build):
    sql = build().sql()
    assert "LATERAL" not in sql
    assert '(SELECT COUNT(*) FROM "posts"' in sql and '(SELECT MAX(' in sql


def test_lock_guard_keeps_for_update():
    assert User.objects.lock().select(*_two_posts_aggregates()).sql().endswith('FOR UPDATE OF "users"')


@pytest.mark.parametrize("limit,offset", [(10, None), (None, 5), (10, 5), (0, None)])
def test_paginated_relation_aggregates_keep_scalar_subqueries(limit, offset):
    query = User.objects.select(User.id, func.count(User.posts), func.max(User.posts.views))
    query = query.order_by(User.name, User.id).limit(limit).offset(offset)
    sql = query.sql()
    assert "LATERAL" not in sql
    assert '(SELECT COUNT(*) FROM "posts"' in sql
    assert '(SELECT MAX(' in sql
    assert 'ORDER BY "users"."name" ASC, "users"."id" ASC' in sql
    if limit is not None:
        assert f"LIMIT {limit}" in sql
    if offset is not None:
        assert f"OFFSET {offset}" in sql
    assert query.inspect().statements[0].joins_count == 0
    assert query.inspect().statements[0].correlated_count == 2
    assert "LATERAL" in query.limit(None).offset(None).sql()


def test_select_to_one_columns_join_and_to_many_is_rejected():
    query = Post.objects.select(Post.title, Post.author.name.label("author"))
    assert query.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 1, 0, 0, 0),
    ))
    sql = query.sql()
    assert sql == (
        'SELECT "posts"."title", "j1"."name" FROM "posts" LEFT JOIN "users" AS "j1" ON "j1"."id" = "posts"."author_id"'
    )
    with pytest.raises(QueryError, match="aggregate it"):
        User.objects.select(User.posts.title).sql()


def test_subquery_and_distinct():
    inner = User.objects.filter(User.name == "A").select(User.id)
    assert Post.objects.filter(Post.author_id.in_(inner)).inspect().statements[0].correlated_count == 0
    assert Post.objects.filter(Post.author_id.in_(inner)).inspect().statements[0].uncorrelated_count == 1
    assert where(Post.objects.filter(Post.author_id.in_(inner))) == (
        '"posts"."author_id" IN (SELECT "users"."id" FROM "users" WHERE "users"."name" = \'A\')'
    )
    assert "NOT IN (SELECT" in where(Post.objects.filter(Post.author_id.not_in(inner)))
    with pytest.raises(QueryError, match="exactly one column"):
        Post.objects.filter(Post.author_id.in_(User.objects.select(User.id, User.name))).sql()
    assert Post.objects.select(Post.author_id).distinct().sql().startswith('SELECT DISTINCT "posts"."author_id"')
    sql = Post.objects.select(Post.title).distinct(Post.author_id).order_by(Post.author_id, Post.views.desc()).sql()
    assert sql.startswith('SELECT DISTINCT ON ("posts"."author_id") "posts"."title" FROM "posts"')


def test_select_validation():
    with pytest.raises(ValueError, match="label"):
        Post.objects.select(Post.id, User.id)  # both named "id"
    with pytest.raises(TypeError):
        Post.objects.select(User)
    with pytest.raises(TypeError):
        Post.objects.select(1)
    with pytest.raises(QueryError):
        Post.objects.load(Post.author).select(Post.id)
    with pytest.raises(QueryError, match="unknown function"):
        Post.objects.select(orm_func("nope", Post.id)).sql()
    with pytest.raises(QueryError, match="lock"):
        Post.objects.lock().select(func.count()).sql()


def orm_func(name, *args):
    from orm.expr import Func

    return Func(name, args)


# -- subqueries, windows, CTEs ------------------------------------------------------------------


def test_exists_with_outer_correlates():
    from orm import exists, outer

    query = User.objects.filter(exists(Post.objects.filter(Post.author_id == outer(User.id))))
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert w == 'EXISTS(SELECT 1 FROM "posts" WHERE "posts"."author_id" = "users"."id")'


def test_subquery_over_the_same_table_gets_an_alias():
    from orm import outer

    avg = Post.objects.filter(Post.author_id == outer(Post.author_id)).select(func.avg(Post.views)).as_scalar()
    query = Post.objects.filter(Post.views > avg)
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert 'FROM "posts" AS "s1" WHERE "s1"."author_id" = "posts"."author_id"' in w


def test_window_function_sql():
    sql = Post.objects.select(
        func.sum(Post.views).over(partition_by=Post.author_id, order_by=Post.created_at.desc(), rows=(-2, 0))
    ).sql()
    assert sql.startswith(
        'SELECT CAST(SUM("posts"."views") OVER (PARTITION BY "posts"."author_id" '
        'ORDER BY "posts"."created_at" DESC ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS BIGINT)'
    )
    assert "NTILE(4) OVER ()" in Post.objects.select(func.ntile(4).over()).sql()


def test_cte_sql():
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    query = User.objects.filter(User.id.in_(totals.select(totals.c.author_id).filter(totals.c.n > 2)))
    assert query.inspect().statements[0].correlated_count == 0
    assert query.inspect().statements[0].uncorrelated_count == 1
    assert query.inspect().statements[0].cte_count == 1
    sql = query.sql()
    assert sql.startswith(
        'WITH "totals" ("author_id", "n") AS (SELECT "posts"."author_id" AS "author_id", COUNT(*) AS "n" '
        'FROM "posts" GROUP BY "posts"."author_id") SELECT'
    )
    assert sql.endswith('IN (SELECT "totals"."author_id" FROM "totals" WHERE "totals"."n" > 2)')


def test_recursive_cte_joins_itself():
    chain = User.objects.filter(User.id == 1).cte("chain", recursive=lambda c: User.objects.filter(User.id == c.c.id + 1))
    query = User.objects.from_(chain)
    assert query.inspect().statements[0].correlated_count == 0
    assert query.inspect().statements[0].uncorrelated_count == 0
    assert query.inspect().statements[0].cte_count == 1
    sql = query.sql()
    assert sql.startswith('WITH RECURSIVE "chain"')
    assert 'FROM "users", "chain" WHERE "users"."id" = "chain"."id" + 1' in sql
    assert sql.endswith('FROM "chain"')


def test_many_to_many_filter_is_one_exists_through_the_join_table():
    from blog.models import Tag

    query = Post.objects.filter(Post.tags.name == "x")
    assert query.inspect().statements[0].joins_count == 1
    assert query.inspect().statements[0].correlated_count == 1
    assert query.inspect().statements[0].uncorrelated_count == 0
    w = where(query)
    assert w == (
        'EXISTS(SELECT 1 FROM "tags" AS "t1" INNER JOIN "post_tags" AS "t2" ON "t2"."tag_id" = "t1"."id" '
        "WHERE \"t2\".\"post_id\" = \"posts\".\"id\" AND \"t1\".\"name\" = 'x')"
    )
    sql = Post.objects.select(func.count(Post.tags)).sql()
    assert '(SELECT COUNT(*) FROM "tags" AS "a1" INNER JOIN "post_tags" AS "a2" ON "a2"."tag_id" = "a1"."id" ' \
           'WHERE "a2"."post_id" = "posts"."id")' in sql
    assert Tag.objects.filter(Tag.posts.author.name == "A").inspect().statements[0].correlated_count == 2


def test_has_one_joins_on_the_other_side():
    query = User.objects.load(User.profile)
    assert query.inspect() == orm.QueryInspection(1, False, (
        orm.StatementInspection(False, 1, 0, 0, 0),
    ))
    sql = query.sql()
    assert 'LEFT JOIN "profiles" AS "j1" ON "j1"."user_id" = "users"."id"' in sql


def test_cte_reads_each_column_once(tmp_path):
    import orm

    (tmp_path / "loud.toml").write_text('name = "hstore"\n[types.loud]\nsql = "text"\nvalue = "text"\nread = "({} || \'!\')"\n')
    source = (
        f'import "{tmp_path / "loud.toml"}"\n'
        'datasource db {\n  provider = "postgresql"\n  extensions = [hstore]\n}\n'
        'model Doc {\n  id Int @id\n  word Unsupported("loud")\n}\n'
    )
    Doc = orm.loads(source, registry=orm.Registry())["Doc"]
    sql = Doc.objects.from_(Doc.objects.filter(Doc.id > 0).cte("recent")).sql()
    assert sql.count("|| '!'") == 1, sql


def test_minus_column_is_a_descending_order():
    assert Post.objects.order_by(-Post.created_at, Post.id).sql() == Post.objects.order_by(Post.created_at.desc(), Post.id).sql()
    assert Comment.objects.order_by(-Comment.post.title).sql().endswith('ORDER BY "j1"."title" DESC')
    assert repr(-Post.views) == "Post.views.desc()"
    with pytest.raises(TypeError, match="expected a condition"):
        Post.objects.filter(-Post.published)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="select\\(\\) takes columns and expressions"):
        Post.objects.select(-Post.views)  # type: ignore[call-overload]
    with pytest.raises(TypeError):
        -(Post.views + 1)  # type: ignore[operator]


def test_nulls_first_and_last():
    assert Comment.objects.order_by(Comment.author_id.desc(nulls="last")).sql().endswith('ORDER BY "comments"."author_id" DESC NULLS LAST')
    assert Comment.objects.order_by(Comment.author_id.asc(nulls="first"), Comment.id).sql().endswith('ORDER BY "comments"."author_id" ASC NULLS FIRST, "comments"."id" ASC')
    assert repr(Comment.author_id.asc(nulls="first").reversed()) == "Comment.author_id.desc(nulls='last')"
    with pytest.raises(ValueError, match="nulls is 'first' or 'last'"):
        Comment.author_id.desc(nulls="middle")  # type: ignore[arg-type]


def test_string_functions_and_concatenation():
    sql = Post.objects.select(
        func.concat(Post.title, " by ", Post.views), Post.title.concat("!"), func.trim(Post.title),
        func.ltrim(Post.title), func.rtrim(Post.title), func.replace(Post.title, "a", "b"),
        func.substr(Post.title, 2, 3), func.strpos(Post.title, "x"),
    ).sql()
    assert sql == (
        "SELECT CONCAT(\"posts\".\"title\", ' by ', \"posts\".\"views\"), \"posts\".\"title\" || '!', "
        "TRIM(\"posts\".\"title\"), LTRIM(\"posts\".\"title\"), RTRIM(\"posts\".\"title\"), "
        "REPLACE(\"posts\".\"title\", 'a', 'b'), SUBSTR(\"posts\".\"title\", 2, 3), "
        "STRPOS(\"posts\".\"title\", 'x') FROM \"posts\""
    )
    assert where(Post.objects.filter(Post.title.concat(Post.body) == "ab")) == "(\"posts\".\"title\" || \"posts\".\"body\") = 'ab'"
    with pytest.raises(TypeError, match="at least one"):
        func.concat()


def test_array_element_and_unnest():
    sql = Profile.objects.select(Profile.links[1], func.unnest(Profile.links)).sql()
    assert sql == "SELECT (\"profiles\".\"links\")[1], UNNEST(\"profiles\".\"links\") FROM \"profiles\""
    with pytest.raises(QueryError, match="only be a select\\(\\) column"):
        Profile.objects.filter(func.unnest(Profile.links) == "x").sql()
    with pytest.raises(QueryError, match="an index needs an array"):
        Post.objects.select(Post.title[1]).sql()  # type: ignore[index]
    with pytest.raises(TypeError, match="not iterable"):
        list(Profile.links)


def test_case_expression():
    sql = Post.objects.select(Post.author_id, func.sum(func.case((Post.published, 1), default=0)).label("n")).group_by(Post.author_id).sql()
    assert sql == (
        'SELECT "posts"."author_id", CAST(SUM(CASE WHEN "posts"."published" = TRUE THEN 1 ELSE 0 END) AS BIGINT) '
        'FROM "posts" GROUP BY "posts"."author_id"'
    )
    heat = func.case((Post.views > 100, "hot"), (Post.views > 10, "warm"), default="cold")
    assert where(Post.objects.filter(heat == "hot")) == (
        '(CASE WHEN "posts"."views" > 100 THEN \'hot\' WHEN "posts"."views" > 10 THEN \'warm\' ELSE \'cold\' END) = \'hot\''
    )
    assert Post.objects.order_by(func.case((Post.published, 0), default=1)).sql().endswith(
        'ORDER BY CASE WHEN "posts"."published" = TRUE THEN 0 ELSE 1 END ASC'
    )
    # without a default: ELSE NULL; a to-many path in a condition is an EXISTS
    assert where(User.objects.filter(func.case((User.posts.views > 3, User.name)) == "x")) == (
        'EXISTS(SELECT 1 FROM "posts" AS "t1" WHERE "t1"."author_id" = "users"."id" '
        'AND (CASE WHEN "t1"."views" > 3 THEN "users"."name" END) = \'x\')'
    )
    with pytest.raises(TypeError, match="at least one"):
        func.case(default=1)
    with pytest.raises(TypeError, match="tuples"):
        func.case(Post.published)  # type: ignore[arg-type]


def test_aggregate_filter():
    sql = Post.objects.select(
        Post.author_id, func.count(filter=Post.published), func.sum(Post.views, filter=Post.views > 10)
    ).group_by(Post.author_id).sql()
    assert sql == (
        'SELECT "posts"."author_id", COUNT(*) FILTER (WHERE "posts"."published" = TRUE), '
        'CAST(SUM("posts"."views") FILTER (WHERE "posts"."views" > 10) AS BIGINT) FROM "posts" GROUP BY "posts"."author_id"'
    )
    # over a relation, the filter is planned inside the correlated subquery
    assert User.objects.select(User.id, func.count(User.posts, filter=User.posts.published)).sql() == (
        'SELECT "users"."id", (SELECT COUNT(*) FILTER (WHERE "a1"."published" = TRUE) FROM "posts" AS "a1" '
        'WHERE "a1"."author_id" = "users"."id") FROM "users"'
    )
    assert 'COUNT(*) FILTER (WHERE "a1"."views" > 3) FROM "posts" AS "a1"' in User.objects.select(func.count(filter=User.posts.views > 3)).sql()
    # before OVER in a window
    assert 'SUM("posts"."views") FILTER (WHERE "posts"."published" = TRUE) OVER (PARTITION BY "posts"."author_id")' in (
        Post.objects.select(func.sum(Post.views, filter=Post.published).over(partition_by=Post.author_id)).sql()
    )
    with pytest.raises(QueryError, match="one relation path"):
        User.objects.select(func.count(User.posts, filter=User.comments.body == "x")).sql()
