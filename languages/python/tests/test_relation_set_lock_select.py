"""A relation query set in `load()`: `lock()` and `select(...).label(...)`."""

import os
from pathlib import Path

import pytest
from blog.models import Comment, Post, User

import monorm
from monorm import QueryError, TransactionRequired, func

SQLITE_SOURCE = Path(__file__).resolve().parents[3].joinpath("examples/sqlite/schema.prisma").read_text()


def postgres() -> bool:
    return "postgres" in os.environ.get("ORM_TEST_DATABASE_URL", "postgres")


async def seed():
    alice, bob, carol = await User.objects.insert_many(
        [{"email": "a@example.com", "name": "Alice"}, {"email": "b@example.com", "name": "Bob"}, {"email": "c@example.com", "name": "Carol"}]
    ).returning()
    await Post.objects.insert_many(
        [
            {"author": alice, "title": "A1", "body": "", "views": 80},
            {"author": alice, "title": "A2", "body": "", "views": 40},
            {"author": bob, "title": "B1", "body": "", "views": 5},
        ]
    )
    return alice, bob, carol


async def test_a_locked_relation_set_locks_its_in_query(clean, db):
    if not postgres():
        pytest.skip("FOR UPDATE needs Postgres")
    await seed()
    events: list[monorm.QueryEvent] = []
    db.on_query(events.append)
    async with db.transaction():
        users = await User.objects.order_by(User.id).load(User.posts.objects.lock())
    sqls = [e.sql for e in events if "FROM posts" in e.sql or 'FROM "posts"' in e.sql]
    assert len(sqls) == 1 and sqls[0].rstrip().endswith('FOR UPDATE OF "posts"')
    assert [len(u.posts.cached) for u in users] == [2, 1, 0]


async def test_a_share_lock_with_skip_locked(clean, db):
    if not postgres():
        pytest.skip("FOR SHARE needs Postgres")
    await seed()
    events: list[monorm.QueryEvent] = []
    db.on_query(events.append)
    async with db.transaction():
        await User.objects.load(User.posts.objects.lock(False, skip_locked=True))
    assert any(e.sql.rstrip().endswith('FOR SHARE OF "posts" SKIP LOCKED') for e in events)


async def test_a_locked_relation_set_needs_a_transaction(clean):
    await seed()
    with pytest.raises(TransactionRequired):
        await User.objects.load(User.posts.objects.lock())
    with pytest.raises(TransactionRequired):
        await User.objects.load(User.posts.comments).load(User.posts.objects.lock())


async def test_a_locked_relation_set_blocks_a_second_lock(clean, db):
    if not postgres():
        pytest.skip("FOR UPDATE needs Postgres")
    await seed()
    async with db.transaction():
        await User.objects.load(User.posts.objects.lock())
        other = await monorm.connect(os.environ["ORM_TEST_DATABASE_URL"], default=False)
        try:
            with pytest.raises(monorm.LockNotAvailable):
                async with other.transaction():
                    await Post.objects.using(other).lock(nowait=True)
        finally:
            await other.close()


async def test_a_lock_cannot_follow_a_per_parent_slice(clean, db):
    await seed()
    async with db.transaction():
        with pytest.raises(QueryError, match="lock\\(\\) can't be used with a slice"):
            await User.objects.load(User.posts.objects.lock()[:1].label("first"))


async def test_a_bound_read_and_prefetch_check_the_transaction(clean):
    alice, _, _ = await seed()
    with pytest.raises(TransactionRequired):
        await monorm.of(alice).load(User.posts.objects.lock())
    with pytest.raises(TransactionRequired):
        await alice.load(User.posts.objects.lock())


async def test_a_selected_relation_set_gives_rows_per_parent(clean):
    await seed()
    users = await User.objects.order_by(User.id).load(
        User.posts.objects.select(Post.id, func.lower(Post.title).label("title")).order_by(Post.id).label("titles")
    )
    assert [[tuple(r)[1] for r in u.titles] for u in users] == [["a1", "a2"], ["b1"], []]
    row = users[0].titles[0]
    assert type(row).__name__ == "Row" and row.title == "a1" and isinstance(row.id, int)


async def test_a_selected_relation_set_filters_orders_and_slices_per_parent(clean):
    await seed()
    users = await User.objects.order_by(User.id).load(
        User.posts.objects.filter(Post.views > 10).select(Post.title).order_by(Post.views.desc())[:1].label("top")
    )
    assert [[r.title for r in u.top] for u in users] == [["A1"], [], []]


async def test_a_grouped_relation_select_groups_per_parent(clean):
    await seed()
    users = await User.objects.order_by(User.id).load(
        User.posts.objects.select(func.count().label("n"), func.sum(Post.views).label("views"))
        .group_by(Post.published)
        .label("stats")
    )
    assert [[(r.n, r.views) for r in u.stats] for u in users] == [[(2, 120)], [(1, 5)], []]


async def test_a_selected_relation_set_loads_beside_another_relation(clean):
    await seed()
    users = await User.objects.order_by(User.id).load(
        User.posts.objects.select(Post.title).order_by(Post.id).label("titles"), User.profile
    )
    assert [len(u.titles) for u in users] == [2, 1, 0]
    assert users[0].profile is None


async def test_a_select_in_load_needs_a_label(clean):
    with pytest.raises(TypeError, match="needs .label"):
        User.objects.load(User.posts.objects.select(Post.id))
    with pytest.raises(TypeError, match="relation query set"):
        User.objects.load(Post.objects.select(Post.id))
    with pytest.raises(TypeError, match="relation query set"):
        Post.objects.select(Post.id).label("x")


async def test_a_selected_relation_set_cannot_lock_or_run_alone(clean, db):
    await seed()
    with pytest.raises(QueryError, match="select\\(\\) can't lock rows"):
        async with db.transaction():
            await User.objects.load(User.posts.objects.lock().select(Post.id).label("ids"))
    with pytest.raises(TypeError, match="inside load"):
        await User.posts.objects.select(Post.id)


async def test_an_instance_load_takes_a_selected_relation_set(clean):
    alice, _, _ = await seed()
    fresh = await User.objects.get(User.id == alice.id)
    await fresh.load(User.posts.objects.select(Post.title).order_by(Post.id).label("titles"))
    assert [r.title for r in fresh.titles] == ["A1", "A2"]


async def test_sqlite_selected_relation_set():
    registry = monorm.Registry()
    m = monorm.loads(SQLITE_SOURCE, registry=registry)
    Author, Book = m["Author"], m["Book"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        a, b = (
            await Author.objects.using(db)
            .insert_many([{"email": "a@example.com", "name": "Alice"}, {"email": "b@example.com", "name": "Bob"}])
            .returning()
        )
        await Book.objects.using(db).insert_many(
            [{"author_id": a.id, "title": "one"}, {"author_id": a.id, "title": "two"}, {"author_id": b.id, "title": "three"}]
        )
        authors = await Author.objects.using(db).order_by(Author.id).load(
            Author.books.objects.select(Book.title).order_by(Book.id).label("titles")
        )
        assert [[r.title for r in x.titles] for x in authors] == [["one", "two"], ["three"]]
        with pytest.raises(TransactionRequired):
            await Author.objects.using(db).load(Author.books.objects.lock())
        grouped = await Author.objects.using(db).order_by(Author.id).load(
            Author.books.objects.select(func.count().label("n")).group_by(Book.status).label("stats")
        )
        assert [[r.n for r in x.stats] for x in grouped] == [[2], [1]]
        counted = await Author.objects.using(db).order_by(Author.id).load(Author.books.objects.select(func.count().label("n")).label("c"))
        assert [[r.n for r in x.c] for x in counted] == [[2], [1]]
    finally:
        await db.drop_tables()
        await db.close()
