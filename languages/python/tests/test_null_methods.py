"""`.or_null()` / `.and_not_null()`: a NULL row kept or dropped, on PostgreSQL and SQLite."""

import pytest
from blog.models import Comment, Post, Profile, User

import orm
from conftest import DATABASE_URL
from orm import QueryError
from orm.expr import Func, differs

SCHEMA = """
model Reading {
  id    BigInt  @id @default(autoincrement())
  score Int?
  name  String?
  tag   String
  meta  Json?
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


def test_a_computed_value_takes_the_method():
    x = Comment.author_id
    assert where(Comment.objects.filter((x + 1 > 5).or_null())).endswith('("comments"."author_id" + 1) IS NULL')
    assert where(Comment.objects.filter((orm.func.abs(x) > 5).and_not_null())).endswith("IS NOT NULL")
    assert where(Comment.objects.filter((orm.func.max(x) > 5).or_null())).endswith("IS NULL")
    for non_null in [orm.func.coalesce(x, 0) > 1, orm.func.count() > 1, orm.func.abs(Comment.id) > 1]:
        for method in (non_null.or_null, non_null.and_not_null):
            with pytest.raises(QueryError, match="the value is NOT NULL"):
                method()
    with pytest.raises(TypeError, match="already a NULL test"):
        orm.func.max(x).is_null().or_null()


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


async def test_a_computed_comparison_keeps_or_drops_a_null_row(models):
    db, Reading, *_ = models
    a, b, null = await Reading.objects.using(db).insert_many(
        [
            {"score": 10, "name": "ab", "tag": "t"},
            {"score": 30, "name": "cd", "tag": "t"},
            {"score": None, "name": None, "tag": "t"},
        ]
    ).returning()

    async def ids(condition):
        return {r.id for r in await Reading.objects.using(db).filter(condition)}

    lower = orm.func.lower(Reading.name)
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows, and_not_null\(\) drops them"):
        await ids(lower != "ab")
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation"):
        ~(lower == "ab")
    assert await ids(lower == "ab") == {a.id}
    assert await ids((lower != "ab").or_null()) == {b.id, null.id}
    assert await ids((lower != "ab").and_not_null()) == {b.id}
    assert await ids(~(lower == "ab").or_null()) == {b.id}
    assert await ids(~(lower == "ab").and_not_null()) == {b.id, null.id}
    assert await ids((Reading.score + 1 > 20).or_null()) == {b.id, null.id}
    # a coalesce with a non-null last argument is provably non-null: no method, no raise
    safe = orm.func.coalesce(Reading.score, 0)
    assert await ids(safe != 10) == {b.id, null.id}
    assert await ids(~(safe == 10)) == {b.id, null.id}
    with pytest.raises(QueryError, match="the value is NOT NULL"):
        (safe != 10).or_null()


def test_the_strict_rule_runs_on_a_computed_value():
    x = Comment.author_id
    fn = orm.func
    assert Comment.objects.filter(fn.coalesce(x, 0) != 3).sql()
    assert Comment.objects.filter(fn.coalesce(x, Comment.id) != 3).sql()
    assert Comment.objects.filter(~(fn.abs(Comment.id) == 3)).sql()
    for build in [
        lambda: Comment.objects.filter(fn.coalesce(x, x) != 3),
        lambda: Comment.objects.filter(fn.abs(x) != 3),
        lambda: Comment.objects.filter(fn.max(x) != 3),
        lambda: Comment.objects.filter(fn.sum(x) != 3),
        lambda: Comment.objects.filter(~(fn.abs(x) == 3)),
        lambda: Comment.objects.exclude(fn.abs(x) == 3),
        lambda: Comment.objects.filter(fn.abs(x).not_in([1, 2])),
    ]:
        with pytest.raises(QueryError, match="or_null"):
            build()


def test_a_scalar_subquery_is_nullable():
    latest = Post.objects.filter(Post.author_id == orm.outer(User.id)).select(Post.id)[:1].as_scalar()
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        User.objects.filter(latest != 3)
    assert User.objects.filter((latest != 3).or_null()).sql()
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation"):
        ~(latest == 3)


def _computed_values():
    def sub(n):
        return Post.objects.filter(Post.author_id == orm.outer(User.id)).select(Post.id)[:n].as_scalar()

    def window(n):
        return orm.func.lag(Comment.author_id, n).over(order_by=Comment.id)

    def kase(n):
        return orm.func.case((Comment.author_id > n, 1))

    return [("subquery", User, sub(1), sub(2)), ("case", Comment, kase(1), kase(2)), ("window", Comment, window(1), window(2))]


def test_two_different_computed_values_are_two_values_for_the_strict_rule():
    for label, model, a, b in _computed_values():
        with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
            model.objects.filter((a != 3) | b.is_null())
        assert model.objects.filter((a != 3) | a.is_null()) is not None, label


def test_the_message_names_a_subquery_a_case_and_a_window_value():
    named = {"subquery": ".as_scalar()", "case": "case(", "window": "lag("}
    for label, model, a, _ in _computed_values():
        with pytest.raises(QueryError) as raised:
            model.objects.filter(a != 3)
        assert "object at 0x" not in str(raised.value), label
        assert named[label] in str(raised.value), (label, str(raised.value))


def test_a_having_condition_on_an_aggregate_follows_the_rule():
    fn, x = orm.func, Comment.author_id
    q = Comment.objects.select(x, fn.count()).group_by(x)
    assert q.having(fn.count() != 0).sql()
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        q.having(fn.max(Comment.id) != 3)
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation"):
        q.having(~(fn.sum(Comment.id) > 3))
    assert q.having((fn.max(Comment.id) != 3).or_null()).sql()


def test_the_strict_rule_runs_in_a_join_on_and_an_aggregate_filter_on_a_computed_value():
    fn = orm.func
    totals = Post.objects.select(Post.author_id, fn.sum(Post.views).label("views")).group_by(Post.author_id).cte("totals")
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        Comment.objects.join(totals, fn.abs(Comment.author_id) != 1)
    assert Comment.objects.join(totals, fn.coalesce(Comment.author_id, 0) != 1).sql()
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        fn.count(filter=fn.abs(Comment.author_id) != 1)
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation"):
        fn.count(filter=~(fn.max(Comment.id) == 1))
    assert fn.count(filter=fn.coalesce(Comment.author_id, 0) != 1) is not None


def test_the_strict_rule_does_not_run_in_case_or_select():
    fn, x = orm.func, Comment.author_id
    assert Comment.objects.select(fn.case((fn.abs(x) != 1, 1), default=0)).sql()
    assert Comment.objects.select((fn.abs(x) != 1).label("different")).sql()


def test_a_path_null_rule_reaches_a_condition_nested_in_a_function():
    fn = orm.func
    plain = where(Post.objects.filter(fn.coalesce(Post.author.name == "x", False) == True))  # noqa: E712
    ruled = where(Post.objects.filter(fn.coalesce(Post.author.or_null().name == "x", False) == True))  # noqa: E712
    assert 'NOT "t1"."name" IS NOT NULL' in ruled
    assert "IS NOT NULL" not in plain


def test_a_bound_parameter_and_a_constant_are_not_nullable():
    prepared = Post.objects.filter(Post.id != orm.param("x")).prepare()
    assert prepared.sql(x=3).split(" WHERE ", 1)[1] == '"posts"."id" <> 3'
    assert where(Post.objects.filter(~(orm.func.abs(Post.id) == 3))) == 'NOT (ABS("posts"."id")) = 3'


def test_an_array_index_is_nullable():
    first, fifth = Profile.links[1], Profile.links[5]
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        Profile.objects.filter(fifth != "a")
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation"):
        Profile.objects.filter(~(first == "a"))
    assert where(Profile.objects.filter((fifth == "a").or_null())).endswith("IS NULL")
    assert where(Profile.objects.filter((fifth == "a").and_not_null())).endswith("IS NOT NULL")


def test_a_function_is_nullable_unless_it_is_known_to_keep_a_non_null_input_non_null():
    fn = orm.func
    for kept in [fn.lower(User.email), fn.length(User.email), fn.concat(User.email, "x"), fn.abs(Comment.id)]:
        with pytest.raises(QueryError, match="the value is NOT NULL"):
            (kept == "a").or_null()
        assert Comment.objects.filter(kept != "a") is not None
    for unknown in [Func("round", (Comment.id,)), Func("sign", (Comment.id,)), fn.sum(Comment.id)]:
        with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
            Comment.objects.filter(unknown != 1)
        assert Comment.objects.filter((unknown != 1).or_null()) is not None


def test_a_division_is_nullable_and_stays_non_strict():
    x = Comment.id
    assert where(Comment.objects.filter((x / 2 > 1).or_null())).endswith("IS NULL")
    assert Comment.objects.filter(x / 2 != 1) is not None
    with pytest.raises(QueryError, match="the value is NOT NULL"):
        (x + 2 > 1).or_null()


def test_a_value_over_a_to_one_path_is_nullable_and_flips_like_the_column():
    lower = orm.func.lower
    column = where(Comment.objects.filter(Comment.author.name != "x"))
    assert "NOT EXISTS" in column
    assert where(Comment.objects.filter(lower(Comment.author.name) != "x")).count("NOT EXISTS") == 1
    assert where(Comment.objects.filter(lower(Comment.author.name).not_in(["x"]))).count("NOT EXISTS") == 1
    assert Comment.objects.filter((lower(Comment.author.name) != "x").or_null()) is not None


async def test_a_json_path_is_strict(models):
    db, Reading, *_ = models
    if db.url.startswith("sqlite://"):
        pytest.skip("sqlite does not support JSON paths")
    a, b, null = await Reading.objects.using(db).insert_many(
        [
            {"name": "a", "tag": "t", "meta": {"k": 1}},
            {"name": "b", "tag": "t", "meta": {"k": 2}},
            {"name": "c", "tag": "t", "meta": None},
        ]
    ).returning()
    qs = Reading.objects.using(db)

    async def ids(condition):
        return {r.id for r in await qs.filter(condition)}

    path = Reading.meta["k"]
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        qs.filter(path != 1)
    with pytest.raises(QueryError, match=r"or_null\(\) drops NULL rows under negation"):
        qs.filter(~(path == 1))
    assert await ids((path != 1).or_null()) == {b.id, null.id}
    assert await ids((path != 1).and_not_null()) == {b.id}


def test_outer_is_strict():
    outer = orm.outer(Comment.author_id)
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        Post.objects.filter(Post.id != outer)
    assert Post.objects.filter((Post.id != outer).or_null()) is not None
    assert Post.objects.filter(Post.id != orm.outer(Comment.id)) is not None


def test_a_path_null_rule_reaches_the_whole_value_not_only_the_column():
    fn = orm.func
    author = Comment.author.or_null()
    plain = where(Comment.objects.filter(fn.lower(Comment.author.name) == "x"))
    ruled = where(Comment.objects.filter(fn.lower(author.name) == "x"))
    assert "IS NOT NULL" not in plain
    assert 'NOT EXISTS(SELECT 1 FROM "users" AS "t1" WHERE "t1"."id" = "comments"."author_id" AND (LOWER("t1"."name")) IS NOT NULL)' in ruled


async def test_the_strict_rule_runs_in_having_a_join_on_and_an_aggregate_filter_on_rows(models):
    db, Reading, *_ = models
    fn = orm.func
    await Reading.objects.using(db).insert_many(
        [
            {"score": 10, "name": "a", "tag": "g1"},
            {"score": None, "name": None, "tag": "g2"},
        ]
    )
    qs = Reading.objects.using(db)
    grouped = qs.select(Reading.tag).group_by(Reading.tag)

    async def tags(query):
        return {tuple(r)[0] for r in await query}

    top = fn.max(Reading.score)
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        grouped.having(top != 10)
    assert await tags(grouped.having((top != 10).or_null())) == {"g2"}
    assert await tags(grouped.having((top != 10).and_not_null())) == set()
    assert await tags(grouped.having(~(top == 10).and_not_null())) == {"g2"}

    totals = Reading.objects.select(Reading.tag, fn.max(Reading.score).label("m")).group_by(Reading.tag).cte("totals")
    lower = fn.lower(Reading.name)
    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        qs.join(totals, (totals.c.tag == Reading.tag) & (lower != "a"))
    joined = qs.join(totals, (totals.c.tag == Reading.tag) & (lower != "a").or_null()).select(Reading.tag)
    assert await tags(joined) == {"g2"}
    joined = qs.join(totals, (totals.c.tag == Reading.tag) & (lower != "a").and_not_null()).select(Reading.tag)
    assert await tags(joined) == set()

    with pytest.raises(QueryError, match=r"or_null\(\) keeps NULL rows"):
        fn.count(filter=lower != "a")
    for method, expected in (("or_null", 1), ("and_not_null", 0)):
        n = fn.count(filter=getattr(lower != "a", method)()).label("n")
        assert [tuple(r) for r in await qs.select(n)] == [(expected,)], method
