"""Static typing checks for the generated stubs; run by languages/python/tests/test_typing.py with mypy
and pyright. Never executed."""

from datetime import datetime, timedelta, timezone
from decimal import Decimal
from typing import Any, assert_type

from blog.models import Comment, Post, PostInsert, PostQuerySet, Priority, Profile, Role, Tag, User, UserQuerySet

from orm import (
    BoundQuerySet,
    of,
    Case,
    ColumnRef,
    Expression,
    Func,
    Condition,
    ManyRelatedSet,
    OneRowQuery,
    PreparedOneRowQuery,
    Ordering,
    Page,
    Prepared,
    QuerySet,
    QueryInspection,
    RelatedSet,
    Row,
    TsVector,
    Window,
    WindowDef,
    excluded,
    exists,
    func,
    outer,
    param,
    window,
)
from orm import get_database as orm_db


async def check() -> None:
    yesterday = datetime.now(timezone.utc) - timedelta(days=1)

    # Class access gives columns, relation paths continue into the related model.
    assert_type(User.email, ColumnRef[str])
    assert_type(User.posts.created_at, ColumnRef[datetime])
    # A relation query set is the target's query-set class.
    assert_type(User.posts.objects, PostQuerySet)
    assert_type(User.posts.objects.filter(Post.published)[:3].label("top"), PostQuerySet)
    assert_type(await Post.objects.load(Post.title, Post.author.name, Post.comments.objects.as_prefetch()), list[Post])
    assert_type(await Post.objects.load(func.count(Post.comments).label("n")), list[Post])
    assert_type(User.posts.author.email, ColumnRef[str])
    assert_type(Comment.author_id, ColumnRef[int | None])
    assert_type(User.posts.created_at < yesterday, Condition)

    qs = User.objects.filter(User.posts.created_at < yesterday)
    assert_type(qs, UserQuerySet)
    users = await qs
    assert_type(users, list[User])

    # Instance access gives values and related objects.
    u = users[0]
    assert_type(u.email, str)
    assert_type(u.posts, RelatedSet[Post])
    assert_type(u.posts | Post.objects, QuerySet[Post])
    assert_type(await u.posts, list[Post])
    post = await Post.objects.load(Post.author).require().get(Post.id == 1)
    assert_type(post.author, User)
    c = await Comment.objects.first()
    if c is not None:
        assert_type(c.author, User | None)
    assert_type(await User.objects.count(), int)
    assert_type(await User.objects.insert_many([{"email": "a", "name": "A"}], batch_size=10).returning(), list[User])
    assert_type(await User.objects.insert_many([{"email": "a", "name": "A"}], copy=True), int)
    assert_type(await User.objects.get_or_insert(email="a", defaults={"name": "A"}), tuple[User, bool])
    await User.objects.get_or_insert(emial="a")  # E: a misspelled lookup key
    await User.objects.get_or_insert(email=1)  # E: a lookup value of the wrong type
    await User.objects.get_or_insert(email="a", defaults={"nme": "A"})  # E: a misspelled default key
    await User.objects.insert(email="a", name="A").on_conflict(User.email, where=User.name == "A", update=False)
    async for p in Post.objects.order_by(Post.created_at.desc())[:10]:
        assert_type(p, Post)
    assert_type(-Post.created_at, Ordering)
    page = await Post.objects.order_by(-Post.created_at).paginate(first=20)
    assert_type(page, Page[Post])
    assert_type(page.next_cursor, str | None)
    Post.objects.order_by(-Post.created_at, Post.id)
    Post.objects.filter(-Post.published)  # E: an ordering is no condition
    -(Post.views + 1)  # E: only a column has a descending short form

    # Writes are explicit statements.
    alice = await User.objects.insert(email="a@b.c", name="A")
    assert_type(alice, User)
    rows: list[PostInsert] = [{"author": alice, "title": "t", "body": "b"}]
    assert_type(await Post.objects.insert_many(rows).returning(), list[Post])
    upserted = await User.objects.insert(email="a@b.c", name="A2").on_conflict(User.email, update=True).returning()
    assert_type(upserted, User)
    skipped = await User.objects.insert(email="a@b.c", name="A").on_conflict(User.email, update=False).returning()
    assert_type(skipped, User | None)
    assert_type(await Post.objects.insert_many(rows), int)
    assert_type(await Post.objects.insert_many(rows).on_conflict(Post.id, update=False).returning(), list[Post])
    assert_type(await User.objects.insert(email="a@b.c", name="A").on_conflict(User.email, update=True), int)
    flag = alice.name == "A"
    assert_type(
        await User.objects.insert(email="a@b.c", name="A").on_conflict(User.email, update=flag).returning(), User | None
    )
    User.objects.insert(email="a@b.c", name="A").on_conflict(User.email)  # E: update is required
    User.objects.insert(email="a@b.c", name="A").returning()  # E: a plain insert gives the instance
    assert_type(await Post.objects.filter(Post.id == 1).update(views=Post.views + 1), int)
    assert_type(await Post.objects.filter(Post.id == 1).update(views=1).returning(), list[Post])
    assert_type(await Post.objects.filter(Post.id == 1).delete(), int)
    assert_type(await Post.objects.filter(Post.id == 1).delete().returning(), list[Post])
    bumped = (
        await Post.objects.insert(author=alice, title="t", body="b")
        .on_conflict(Post.id, update=True, update_values={"views": Post.views + excluded(Post.views)})
        .returning()
    )
    assert_type(bumped, Post)
    assert_type(await Post.objects.update_many([{"id": 1, "views": 2}]), int)
    assert_type(await Post.objects.update_many([{"id": 1, "author": alice}]).returning(), list[Post])
    grouped = await Post.objects.select(Post.author_id, func.count(), func.sum(Post.views)).group_by(Post.author_id)
    assert_type(grouped, list[Row[int, int, int | None]])
    aid, n, total = grouped[0]
    assert_type((aid, n, total), tuple[int, int, int | None])
    assert_type(grouped[0][1], int)
    grouped[0].anything  # untyped name access
    assert_type(await Post.objects.select(func.max(Post.views)).scalar(), int | None)
    assert_type(
        await Post.objects.select(func.array_agg(Post.title, order_by=Post.views.desc())).scalar(), list[str] | None
    )
    assert_type(await Post.objects.select(Post.title).scalars(), list[str])
    pairs = await User.objects.select(User, func.count(User.posts))
    assert_type(pairs[0][0], User)
    async for batch in Post.objects.batches(100):
        assert_type(batch, list[Post])
    async for one in Post.objects.iterate():
        assert_type(one, Post)
    Post.objects.filter(Post.author_id.in_(User.objects.select(User.id)))
    assert_type(Post.objects.lock(exclusive=False, skip_locked=True), PostQuerySet)
    assert_type(await Post.objects.lock().get(Post.id == 1), Post | None)
    assert_type(await orm_db().lock("key", nowait=True), bool)
    assert_type(await post.qs().lock(skip_locked=True), list[Post])
    assert_type(Post.author == alice, Condition)
    assert_type(Post.author != None, Condition)  # noqa: E711
    Comment.objects.filter(Comment.post.author != alice)
    await post.update(title="new", views=Post.views + 1)
    await alice.posts.insert(title="t", body="b")
    await post.delete()

    # in_bulk, subqueries, windows, CTEs, prefetch.
    assert_type(await Post.objects.in_bulk([1, 2]), dict[Any, Post])
    assert_type(exists(Post.objects.filter(Post.author_id == outer(User.id))), Condition)
    latest = Post.objects.filter(Post.author_id == outer(User.id)).select(Post.title)[:1].as_scalar()
    assert_type(await User.objects.select(User, latest), list[Row[User, str]])
    rank = func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
    assert_type(rank, Window[int])
    assert_type(func.lag(Post.views).over(order_by=Post.id), Window[int | None])
    ranked = Post.objects.select(Post, rank.label("rank")).cte("ranked")
    assert_type(await Post.objects.from_(ranked).filter(ranked.c.rank <= 3), list[Post])
    await ranked.select(ranked.c.author_id).group_by(ranked.c.author_id)
    w = window(partition_by=Post.author_id, order_by=Post.created_at)
    assert_type(w, WindowDef)
    assert_type(func.sum(Post.views).over(w), Window[int | None])
    assert_type(func.row_number().over(w, rows=(None, 0)), Window[int])
    totals = Post.objects.select(Post.author_id, func.count().label("n")).group_by(Post.author_id).cte("totals")
    assert_type(User.objects.join(totals, totals.c.author_id == User.id, outer=True), UserQuerySet)
    assert_type(
        await User.objects.load(User.posts.comments, User.posts.objects.all()[:3].label("top")),
        list[User],
    )

    # Decimal, enum and array columns; one-to-one and many-to-many relations.
    assert_type(Profile.balance, ColumnRef[Decimal])
    assert_type(User.profile.role, ColumnRef[Role])
    assert_type(Tag.priority, ColumnRef[Priority])
    assert_type(Profile.links, ColumnRef[list[str]])
    assert_type(Profile.links.has("x"), Condition)
    assert_type(Post.tags.name, ColumnRef[str])
    prof = await Profile.objects.insert(user=alice, balance=Decimal("1.50"), role=Role.admin, links=["a"])
    assert_type(prof.balance, Decimal)
    assert_type(prof.role, Role)
    assert_type(prof.links, list[str])
    assert_type(await Profile.objects.select(func.sum(Profile.balance)).scalar(), Decimal | None)
    assert_type(await Profile.objects.select(func.avg(Profile.balance)).scalar(), Decimal | None)
    assert_type(func.cardinality(Profile.links), Func[int])
    assert_type(Profile.links[1], Func[str | None])
    assert_type(func.unnest(Profile.links), Func[str])
    assert_type(func.concat(Post.title, " ", Post.views), Func[str])
    assert_type(Post.title.concat("!"), Expression[str])
    assert_type(Profile.links[1].concat("!"), Expression[str | None])
    assert_type(Post.title.concat(Profile.links[1]), Expression[str | None])
    assert_type(func.strpos(Post.title, "x"), Func[int])
    assert_type(func.strpos(Post.title, Profile.links[1]), Func[int])
    assert_type(func.replace(Post.title, Profile.links[1], Profile.links[1]), Func[str])
    Post.views.concat("!")  # E: concatenation takes strings
    assert_type(func.case((Post.published, 1), default=0), Case[int])
    assert_type(func.case((Post.views > 3, Post.title)), Case[str | None])
    assert_type(await Post.objects.select(func.case((Post.published, Post.views), default=0)).scalar(), int | None)
    assert_type(func.count(filter=Post.published), Func[int])
    assert_type(func.sum(Post.views, filter=Post.views > 3), Func[int | None])
    assert_type(func.to_tsvector("english", Post.title), Func[TsVector])
    assert_type(func.ts_rank(func.to_tsvector(Post.title), func.plainto_tsquery("dog")), Func[float])
    assert_type(func.to_tsvector(Post.title).matches("dog"), Condition)
    Post.title.matches("dog")  # E: matches() needs a tsvector
    assert_type(alice.profile, Profile | None)
    assert_type(post.tags, ManyRelatedSet[Tag])
    assert_type(await post.tags, list[Tag])
    await post.tags.add(await Tag.objects.require().get(Tag.id == 1))
    assert_type(await post.tags.remove(1), int)
    assert_type(await post.tags.insert(name="go"), Tag)
    assert_type(await Post.objects.load(Post.tags), list[Post])

    # Prepared queries keep the model type.
    by_author = Post.objects.filter(Post.author_id == param("a")).limit(param("n")).prepare()
    assert_type(by_author, Prepared[Post])
    assert_type(await by_author(a=1, n=10), list[Post])
    assert_type(await by_author.get(a=1, n=10), Post | None)
    assert_type(await by_author.first(a=1, n=10), Post | None)
    assert_type(await by_author.count(a=1, n=10), int)


async def errors() -> None:
    User.email < 1  # E: ordering a str column against an int
    User.posts.nope  # E: no such column
    User.objects.filter(User.posts.views.like("x"))  # E: like on int column
    await User.objects.insert(email="a@b.c")  # E: missing name
    await User.objects.insert(email="a@b.c", name="A", nope=1)  # E: unknown field
    await Post.objects.update(views="many")  # E: wrong type
    await Post.objects.update(nope=1).returning()  # E: unknown field
    await Post.objects.update_many([{"views": 2}])  # E: missing primary key
    await Post.objects.select(Post.id, Post.title).scalars()  # E: scalars() needs one column
    func.lower(Post.views)  # E: lower() of an int column
    await Post.objects.update_many([{"id": 1, "views": Post.views + 1}])  # E: no expressions
    u = await User.objects.require().get(User.id == 1)
    u.name = "B"  # E: instances are read-only
    await u.update(name=1)  # E: wrong type
    u.posts = []  # E: read-only relation
    User.objects.load(1)  # E: not a column, relation or relation query set
    User.objects.load(User.posts.objects[0].label("chosen"))
    User.objects.load(func.count(User.posts))  # E: an aggregate in load() needs a label
    User.posts.objects.label("old", one=True)  # E: one-row loads use [n].label()
    func.ntile("2")  # E: buckets are ints
    func.sum(Post.views).over(window(), rows=(None, "x"))  # E: frame bounds are ints
    await Profile.objects.insert(user_id=1, role="boss")  # E: roles are Role members
    Post.views.has(1)  # E: not an array
    await Profile.objects.insert(user_id=1, balance=1.5)  # E: a Decimal


def inspection_types() -> None:
    from orm import StatementInspection

    result = User.objects.all().inspect()
    assert_type(result, QueryInspection)
    assert_type(result.statement_count, int)
    assert_type(result.has_batching, bool)
    assert_type(result.statements[0], StatementInspection)
    assert_type(result.statements[0].set_correlated, int)
    assert_type(result.statements[0].set_uncorrelated, int)
    assert_type(result.statements[0].cte_count, int)
    assert_type(User.objects.select(User.id).inspect(), QueryInspection)
    assert_type(User.objects.update(name="A").inspect(), QueryInspection)
    assert_type(User.objects.insert_many([]).inspect(), QueryInspection)
    assert_type(User.objects.update_many([]).returning().inspect(), QueryInspection)


async def required_results() -> None:
    assert_type(Post.objects.first(), OneRowQuery[Post | None])
    assert_type(Post.objects.last(), OneRowQuery[Post | None])
    assert_type(Post.objects.get(), OneRowQuery[Post | None])
    assert_type(Post.objects[0], OneRowQuery[Post | None])
    assert_type(Post.objects[:2], PostQuerySet)
    assert_type(await Post.objects[-1], Post | None)
    assert_type(Post.objects.require()[0], OneRowQuery[Post])
    assert_type(await Post.objects.require()[:2][0], Post)
    assert_type(Post.objects.require().first(), OneRowQuery[Post])
    assert_type(Post.objects.first().prepare(), PreparedOneRowQuery[Post | None])
    assert_type(await Post.objects.require().first().prepare()(), Post)
    assert_type(Post.objects.first().inspect(), QueryInspection)
    assert_type(Post.objects.first().sql(), str)
    bad: Post = await Post.objects[0]  # E: a missing row is optional
    Post.objects.first().filter(Post.id == 1)  # E: a one-row query cannot be filtered
    assert_type(await Post.objects.get(Post.id == 1), Post | None)
    assert_type(await Post.objects.require().get(Post.id == 1), Post)
    assert_type(await Post.objects.require().filter(Post.id == 1).first(), Post)
    assert_type(await Post.objects.require().last(), Post)
    assert_type(await Post.objects.require().all(), list[Post])
    assert_type(await Post.objects.select(Post.title).get(), Row[str] | None)
    assert_type(await Post.objects.select(Post.title).require().get(), Row[str])
    assert_type(await Post.objects.require().select(Post.title).first(), Row[str])
    prepared = Post.objects.filter(Post.id == param("id")).prepare()
    assert_type(await prepared.get(id=1), Post | None)
    assert_type(await prepared.require().get(id=1), Post)
    assert_type(await prepared.require().first(id=1), Post)
    assert_type(await Post.objects.require().filter(Post.id == param("id")).prepare().get(id=1), Post)


def json_paths(meta: ColumnRef[Any]) -> None:
    """A Json column: a path continues with keys and indexes, ``as_text()`` ends it."""
    assert_type(meta["tags"][0].as_text() == "x", Condition)


async def bound_reads(post: Post, posts: list[Post]) -> None:
    """A bound query set reads and fills the given objects; it has no shape-changing method."""
    assert_type(Post.objects.of(posts), BoundQuerySet[Post])
    assert_type(Post.objects.of(post), BoundQuerySet[Post])
    assert_type(of(posts), BoundQuerySet[Post])
    assert_type(post.qs(), BoundQuerySet[Post])
    assert_type(Post.objects.of(posts).filter(Post.id == 1).exclude(Post.id == 2), BoundQuerySet[Post])
    assert_type(
        Post.objects.of(posts).load(Post.title).lock().without_defaults().without_related(), BoundQuerySet[Post]
    )
    assert_type(await Post.objects.of(posts), list[Post])
    assert_type(await Post.objects.of(posts).update(title="x"), list[Post])
    assert_type(await Post.objects.of(posts).delete(), list[Post])
    assert_type(await post.qs().update(views=1), list[Post])
    Post.objects.of(posts).update(nope=1)  # E: not a Post field
    Post.objects.of(posts).hard_delete()  # E: no hard_delete without the soft-delete extension
    Post.objects.of(posts).undelete()  # E: no undelete without the soft-delete extension
    assert_type(await Post.objects.of(posts).count(), int)
    assert_type(await Post.objects.of(posts).exists(), bool)
    assert_type(Post.objects.of(posts).sql(), str)
    Post.objects.of(posts).order_by(Post.id)  # E: no order_by on a bound query set
    Post.objects.of(posts)[:2]  # E: no slice on a bound query set
    Post.objects.of(posts).limit(1)  # E: no limit on a bound query set
    Post.objects.of(posts).offset(1)  # E: no offset on a bound query set
    Post.objects.of(posts).first()  # E: no first on a bound query set
    Post.objects.of(posts).last()  # E: no last on a bound query set
    Post.objects.of(posts).get()  # E: no get on a bound query set
    Post.objects.of(posts).paginate()  # E: no paginate on a bound query set
    Post.objects.of(posts).select(Post.id)  # E: no select on a bound query set
    Post.objects.of(posts).group_by(Post.id)  # E: no group_by on a bound query set
    Post.objects.of(posts).distinct()  # E: no distinct on a bound query set
    Post.objects.of(posts).join(Post.author)  # E: no join on a bound query set
    Post.objects.of(posts).cte("c")  # E: no cte on a bound query set
    Post.objects.of(posts).union(Post.objects)  # E: no union on a bound query set
    Post.objects.of(posts).insert_many([])  # E: no insert_many on a bound query set
    Post.objects.of(posts).in_bulk()  # E: no in_bulk on a bound query set
    Post.objects.of(posts).prepare()  # E: no prepare on a bound query set
    Post.objects.of(posts).require()  # E: no require on a bound query set
    Post.objects.of(posts).label("x")  # E: no label on a bound query set
    async for _ in Post.objects.of(posts):  # E: no async for on a bound query set
        pass
