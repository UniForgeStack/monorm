"""SQLAlchemy 2 ORM, sync (psycopg) and async (asyncpg), written as the SQLAlchemy docs show.

Each operation opens its own session, as a request handler does: reads in `Session()`, writes in `Session.begin()`.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from common import (
    CONCURRENT_OPS_PER_TASK,
    CONCURRENT_TASKS,
    POOL_SIZE,
    comment_rows,
    database_url,
    delete_setup,
    objs,
    page,
    pk_for,
    plain,
    update_range,
    upsert_rows,
)
from contender import POST, TAG, USER, Case, Contender
from sqlalchemy import BigInteger, Boolean, Column, DateTime, ForeignKey, String, Table, Text, delete, func, insert, select, update
from sqlalchemy.dialects import postgresql
from sqlalchemy.dialects.postgresql import insert as pg_insert
from sqlalchemy.orm import DeclarativeBase, Mapped, joinedload, load_only, mapped_column, relationship, selectinload


class Base(DeclarativeBase):
    pass


post_tags = Table(
    "post_tags",
    Base.metadata,
    Column("id", BigInteger, primary_key=True),
    Column("post_id", ForeignKey("posts.id"), nullable=False),
    Column("tag_id", ForeignKey("tags.id"), nullable=False),
)


class User(Base):
    __tablename__ = "users"
    id: Mapped[int] = mapped_column(BigInteger, primary_key=True)
    email: Mapped[str] = mapped_column(String(254), unique=True)
    name: Mapped[str] = mapped_column(String(100))
    created_at: Mapped[dt.datetime] = mapped_column(DateTime(timezone=True))
    posts: Mapped[list[Post]] = relationship(back_populates="author")


class Tag(Base):
    __tablename__ = "tags"
    id: Mapped[int] = mapped_column(BigInteger, primary_key=True)
    name: Mapped[str] = mapped_column(String(50), unique=True)


class Post(Base):
    __tablename__ = "posts"
    id: Mapped[int] = mapped_column(BigInteger, primary_key=True)
    author_id: Mapped[int] = mapped_column(ForeignKey("users.id"))
    category_id: Mapped[int | None] = mapped_column(BigInteger)
    title: Mapped[str] = mapped_column(String(200))
    body: Mapped[str] = mapped_column(Text)
    views: Mapped[int] = mapped_column(default=0)
    published: Mapped[bool] = mapped_column(Boolean, default=False)
    created_at: Mapped[dt.datetime] = mapped_column(DateTime(timezone=True))
    author: Mapped[User] = relationship(back_populates="posts")
    tags: Mapped[list[Tag]] = relationship(secondary=post_tags)


class Comment(Base):
    __tablename__ = "comments"
    id: Mapped[int] = mapped_column(BigInteger, primary_key=True)
    post_id: Mapped[int] = mapped_column(ForeignKey("posts.id"))
    author_id: Mapped[int | None] = mapped_column(ForeignKey("users.id"))
    body: Mapped[str] = mapped_column(Text)
    created_at: Mapped[dt.datetime] = mapped_column(DateTime(timezone=True))


# ---- statements, shared by both modes (the CPU split builds and renders these) ----------------------------------


def _paged(stmt: Any, n: int, step: int, i: int, key: Any = Post.id) -> Any:
    o, _ = page(i, n, step)
    return stmt.order_by(key).offset(o).limit(n)


def _filter(threshold: int = 500) -> Any:
    return select(Post).where(Post.published, Post.views > threshold).order_by(Post.views.desc(), Post.id).limit(20)


def _read_pk(i: int) -> Any:
    return select(Post).where(Post.id == pk_for(i))  # the CPU split builds the query; `Session.get` runs the same statement


def _projection(i: int) -> Any:
    return _paged(select(Post).options(load_only(Post.id, Post.title)), 1000, 1000, i)


def _filter_ne_null(i: int) -> Any:
    # category_id is nullable: `!=` drops the NULL rows; Django's exclude() keeps them, and IS DISTINCT FROM does too
    return select(Post).where(Post.category_id.is_distinct_from(3)).order_by(Post.id).offset((i % 9) * 20).limit(20)


def _count(i: int) -> Any:
    return select(func.count()).select_from(Post).where(Post.published, Post.views > 500)


def _aggregate(i: int) -> Any:
    return select(Post.author_id, func.sum(Post.views).label("total")).group_by(Post.author_id).order_by(Post.author_id)


def _join(i: int) -> Any:
    return _paged(select(Post).options(joinedload(Post.author, innerjoin=True)), 50, 50, i)


def _prefetch(i: int) -> Any:
    return _paged(select(User).options(selectinload(User.posts)), 50, 50, i, User.id)


def _m2m(i: int) -> Any:
    return _paged(select(Post).options(selectinload(Post.tags)), 50, 50, i)


def _update_1(i: int) -> Any:
    return update(Post).where(Post.id == pk_for(i)).values(views=Post.views + 1)


def _bulk_update(i: int) -> Any:
    lo, hi = update_range(i)
    return update(Post).where(Post.id.between(lo, hi)).values(views=Post.views + 1)


def _upsert(rows: list[dict[str, Any]]) -> Any:
    stmt = pg_insert(User).values(rows)
    # SQLAlchemy reports no rowcount for a multi-row VALUES statement, so the ids come back, as in Django
    return stmt.on_conflict_do_update(index_elements=[User.email], set_={"name": stmt.excluded.name}).returning(User.id)


_INSERT_RETURNING = insert(Comment).returning(Comment, sort_by_parameter_order=True)
_DELETE_100 = delete(Comment).where(Comment.post_id == 1, Comment.body == "del-bench")


def _sql(stmt: Any) -> str:
    return str(stmt.compile(dialect=postgresql.dialect()))


# ---- the contender ------------------------------------------------------------------------------------------------


class SqlalchemyContender(Contender):
    """`asynchronous` picks `AsyncSession` on asyncpg; else `Session` on psycopg 3 (Django's driver)."""

    def __init__(self, asynchronous: bool) -> None:
        self.asynchronous = asynchronous
        self.name = "sqlalchemy-async" if asynchronous else "sqlalchemy-sync"
        self.executor: ThreadPoolExecutor | None = None

    async def start(self) -> None:
        url = database_url().split("://", 1)[1]
        if self.asynchronous:
            from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

            self.engine: Any = create_async_engine(f"postgresql+asyncpg://{url}", pool_size=POOL_SIZE, max_overflow=0)
            self.Session: Any = async_sessionmaker(self.engine, expire_on_commit=False)
            self.cases = self._async_cases()
        else:
            from sqlalchemy import create_engine
            from sqlalchemy.orm import sessionmaker

            self.engine = create_engine(f"postgresql+psycopg://{url}", pool_size=POOL_SIZE, max_overflow=0)
            self.Session = sessionmaker(self.engine, expire_on_commit=False)
            self.executor = ThreadPoolExecutor(CONCURRENT_TASKS)  # outside the timed op: thread start is not query cost
            self.cases = self._sync_cases()

    async def stop(self) -> None:
        if self.executor:
            self.executor.shutdown()
        if self.asynchronous:
            await self.engine.dispose()
        else:
            self.engine.dispose()

    def info(self) -> dict[str, Any]:
        import sqlalchemy

        driver = "asyncpg" if self.asynchronous else "psycopg"
        return {
            "sqlalchemy": sqlalchemy.__version__,
            "driver": driver,
            "prepared_statements": "asyncpg dialect default: a prepared-statement cache per connection (100 statements)"
            if self.asynchronous
            else "psycopg default: server-side prepare after 5 executions of one statement",
            "pool": f"QueuePool pool_size={POOL_SIZE} max_overflow=0, one engine per process",
            "session_note": "one session per operation; the session begins a transaction on first use, so a read runs "
            "BEGIN, SELECT, ROLLBACK and a write runs BEGIN, the statement, COMMIT",
            "insert_note": "bulk-insert-* is insert(Comment).returning(Comment): full instances, as monorm returns",
            "upsert_note": "on_conflict_do_update adds RETURNING id: SQLAlchemy has no rowcount for a multi-row VALUES",
            "update_note": "update()/delete() run with synchronize_session='auto' (the default): bulk-update-100 "
            "falls back to 'fetch' and adds RETURNING id",
            "tx_note": "tx-5 flushes its two inserts as one INSERT statement (insertmanyvalues): 4 statements, not 5",
            **({"async_note": "AsyncSession runs the sync ORM in a greenlet per call"} if self.asynchronous else {}),
        }

    def _norms(self) -> dict[str, Any]:
        def related(fields: tuple[str, ...], rel: str, rel_fields: tuple[str, ...], many: bool) -> Any:
            def norm(raw: list[Any]) -> Any:
                out = []
                for o in raw:
                    r = getattr(o, rel)
                    items = sorted(r, key=lambda x: x.id) if many else [r]
                    out.append([objs([o], fields)[0], objs(items, rel_fields)])
                return out

            return norm

        return {
            "read-pk": lambda raw: objs([raw], POST),
            "read-50": lambda raw: objs(raw, POST),
            "read-1000": lambda raw: objs(raw, POST),
            "projection-1000": lambda raw: [[o.id, o.title] for o in raw],
            "filter": lambda raw: objs(raw, POST),
            "filter-ne-null": lambda raw: objs(raw, POST),
            "count": lambda raw: raw,
            "aggregate": lambda raw: [[plain(r[0]), int(r[1])] for r in raw],
            "join-to-one": related(POST, "author", USER, many=False),
            "prefetch-to-many": related(USER, "posts", POST, many=True),
            "m2m": related(POST, "tags", TAG, many=True),
            "insert-1": lambda raw: raw,
            "bulk-insert-100": lambda raw: [o.id for o in raw],
            "bulk-insert-1000": lambda raw: [o.id for o in raw],
            "update-1": lambda raw: raw,
            "bulk-update-100": lambda raw: raw,
            "upsert-100": lambda raw: len(raw),
            "delete-100": lambda raw: raw,
            "tx-5": lambda raw: objs([raw], POST),
            "concurrent": lambda raw: [r[0] for r in raw],
        }

    def _table(self, ops: dict[str, Any]) -> dict[str, Case]:
        """Join the ops of one mode with the shared norms, setups and CPU-split builders."""
        builders = {
            "read-pk": _read_pk,
            "read-50": lambda i: _paged(select(Post), 50, 50, i),
            "read-1000": lambda i: _paged(select(Post), 1000, 1000, i),
            "projection-1000": _projection,
            "filter": lambda i: _filter(),
            "filter-ne-null": _filter_ne_null,
            "aggregate": _aggregate,
            "join-to-one": _join,
            "prefetch-to-many": _prefetch,
            "m2m": _m2m,
        }
        setups: dict[str, Any] = {
            "insert-1": lambda i, c: comment_rows(i, 1),
            "bulk-insert-100": lambda i, c: comment_rows(i, 100),
            "bulk-insert-1000": lambda i, c: comment_rows(i, 1000),
            "upsert-100": lambda i, c: upsert_rows(i),
            "delete-100": delete_setup,
        }
        norms = self._norms()
        out = {}
        for name, op in ops.items():
            b = builders.get(name)
            case = Case(op=op, norm=norms[name], build=b, sql=(lambda i, b=b: _sql(b(i))) if b else None)
            if name in setups:
                case.setup = setups[name]
            case.concurrent = name == "concurrent"
            out[name] = case
        return out

    def _sync_cases(self) -> dict[str, Case]:
        S = self.Session

        def scalars(stmt: Any) -> list[Any]:
            with S() as s:
                return list(s.scalars(stmt))

        def rows(stmt: Any) -> list[Any]:
            with S() as s:
                return list(s.execute(stmt))

        def read_pk(i: int) -> Any:
            with S() as s:
                return s.get(Post, pk_for(i))

        def count(i: int) -> int:
            with S() as s:
                return s.scalar(_count(i))

        def write(stmt: Any) -> int:
            with S.begin() as s:
                return s.execute(stmt).rowcount

        def upsert(rows_: list[dict[str, Any]]) -> list[Any]:
            with S.begin() as s:
                return list(s.scalars(_upsert(rows_)))

        def insert_1(rows_: list[dict[str, Any]]) -> int:
            c = Comment(**rows_[0])
            with S.begin() as s:
                s.add(c)
            return c.id

        def bulk_insert(rows_: list[dict[str, Any]]) -> list[Any]:
            with S.begin() as s:
                return list(s.scalars(_INSERT_RETURNING, rows_))

        def tx_5(i: int) -> Any:
            p = pk_for(i)
            r = comment_rows(i, 2)
            with S.begin() as s:
                first = Comment(**r[0])
                s.add_all([first, Comment(**r[1])])
                s.flush()
                s.execute(_update_1(i))
                post = s.get(Post, p)
                s.delete(first)
            return post

        def one_task(t: int, i: int) -> tuple[list[Any], list[float]]:
            out, lat = [], []
            for k in range(CONCURRENT_OPS_PER_TASK):
                n = t * CONCURRENT_OPS_PER_TASK + k
                t0 = time.perf_counter()
                with S() as s:
                    if k % 2 == 0:
                        out.append(objs([s.get(Post, pk_for(n + i))], POST))
                    else:
                        out.append(objs(list(s.scalars(_filter(n % 900))), POST))
                lat.append((time.perf_counter() - t0) * 1000)
            return out, lat

        def concurrent(i: int) -> Any:
            assert self.executor
            return list(self.executor.map(lambda t: one_task(t, i), range(CONCURRENT_TASKS)))

        return self._table({
            "read-pk": read_pk,
            "read-50": lambda i: scalars(_paged(select(Post), 50, 50, i)),
            "read-1000": lambda i: scalars(_paged(select(Post), 1000, 1000, i)),
            "projection-1000": lambda i: scalars(_projection(i)),
            "filter": lambda i: scalars(_filter()),
            "filter-ne-null": lambda i: scalars(_filter_ne_null(i)),
            "count": count,
            "aggregate": lambda i: rows(_aggregate(i)),
            "join-to-one": lambda i: scalars(_join(i)),
            "prefetch-to-many": lambda i: scalars(_prefetch(i)),
            "m2m": lambda i: scalars(_m2m(i)),
            "insert-1": insert_1,
            "bulk-insert-100": bulk_insert,
            "bulk-insert-1000": bulk_insert,
            "update-1": lambda i: write(_update_1(i)),
            "bulk-update-100": lambda i: write(_bulk_update(i)),
            "upsert-100": upsert,
            "delete-100": lambda i: write(_DELETE_100),
            "tx-5": tx_5,
            "concurrent": concurrent,
        })

    def _async_cases(self) -> dict[str, Case]:
        S = self.Session

        async def scalars(stmt: Any) -> list[Any]:
            async with S() as s:
                return list(await s.scalars(stmt))

        async def rows(stmt: Any) -> list[Any]:
            async with S() as s:
                return list(await s.execute(stmt))

        async def read_pk(i: int) -> Any:
            async with S() as s:
                return await s.get(Post, pk_for(i))

        async def count(i: int) -> int:
            async with S() as s:
                return await s.scalar(_count(i))

        async def write(stmt: Any) -> int:
            async with S.begin() as s:
                return (await s.execute(stmt)).rowcount

        async def upsert(rows_: list[dict[str, Any]]) -> list[Any]:
            async with S.begin() as s:
                return list(await s.scalars(_upsert(rows_)))

        async def insert_1(rows_: list[dict[str, Any]]) -> int:
            c = Comment(**rows_[0])
            async with S.begin() as s:
                s.add(c)
            return c.id

        async def bulk_insert(rows_: list[dict[str, Any]]) -> list[Any]:
            async with S.begin() as s:
                return list(await s.scalars(_INSERT_RETURNING, rows_))

        async def tx_5(i: int) -> Any:
            p = pk_for(i)
            r = comment_rows(i, 2)
            async with S.begin() as s:
                first = Comment(**r[0])
                s.add_all([first, Comment(**r[1])])
                await s.flush()
                await s.execute(_update_1(i))
                post = await s.get(Post, p)
                await s.delete(first)
            return post

        async def one_task(t: int, i: int) -> tuple[list[Any], list[float]]:
            out, lat = [], []
            for k in range(CONCURRENT_OPS_PER_TASK):
                n = t * CONCURRENT_OPS_PER_TASK + k
                t0 = time.perf_counter()
                async with S() as s:
                    if k % 2 == 0:
                        out.append(objs([await s.get(Post, pk_for(n + i))], POST))
                    else:
                        out.append(objs(list(await s.scalars(_filter(n % 900))), POST))
                lat.append((time.perf_counter() - t0) * 1000)
            return out, lat

        async def concurrent(i: int) -> Any:
            return await asyncio.gather(*(one_task(t, i) for t in range(CONCURRENT_TASKS)))

        return self._table({
            "read-pk": read_pk,
            "read-50": lambda i: scalars(_paged(select(Post), 50, 50, i)),
            "read-1000": lambda i: scalars(_paged(select(Post), 1000, 1000, i)),
            "projection-1000": lambda i: scalars(_projection(i)),
            "filter": lambda i: scalars(_filter()),
            "filter-ne-null": lambda i: scalars(_filter_ne_null(i)),
            "count": count,
            "aggregate": lambda i: rows(_aggregate(i)),
            "join-to-one": lambda i: scalars(_join(i)),
            "prefetch-to-many": lambda i: scalars(_prefetch(i)),
            "m2m": lambda i: scalars(_m2m(i)),
            "insert-1": insert_1,
            "bulk-insert-100": bulk_insert,
            "bulk-insert-1000": bulk_insert,
            "update-1": lambda i: write(_update_1(i)),
            "bulk-update-100": lambda i: write(_bulk_update(i)),
            "upsert-100": upsert,
            "delete-100": lambda i: write(_DELETE_100),
            "tx-5": tx_5,
            "concurrent": concurrent,
        })
