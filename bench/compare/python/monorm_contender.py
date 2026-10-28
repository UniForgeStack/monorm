"""The `orm` package (async), written as docs/api/python-api.md shows."""

from __future__ import annotations

import asyncio
import json
import time
from typing import Any

import monorm
from common import (
    CONCURRENT_OPS_PER_TASK,
    CONCURRENT_TASKS,
    POOL_SIZE,
    ROOT,
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
from monorm import func


class OrmContender(Contender):
    def __init__(self, name: str = "orm") -> None:
        self.name = name

    async def start(self) -> None:
        models = monorm.loads((ROOT / "schema.prisma").read_text())
        self.db = await monorm.connect(database_url(), max_connections=POOL_SIZE)
        global User, Post, Comment, Tag
        User, Post, Comment, Tag = (models[k] for k in ("User", "Post", "Comment", "Tag"))
        self.cases = _cases(self.db)

    async def stop(self) -> None:
        await self.db.close()

    def info(self) -> dict[str, Any]:
        from monorm import _native

        meta = json.loads(_native.profile_metadata())
        return {
            "profile": meta["profile"],
            "build": meta["build"],
            "capabilities": meta["capabilities"],
            "prepared_statements": "engine default: prepared once per connection and cached",
            "pool": POOL_SIZE,
            "insert_note": "monorm returns full rows: insert_many(...).returning() has no key-only form",
            "upsert_note": "on_conflict returns a row count, no RETURNING",
        }


User: Any
Post: Any
Comment: Any
Tag: Any


def _filter(threshold: int = 500) -> Any:
    return Post.objects.filter(Post.published, Post.views > threshold).order_by(-Post.views, Post.id).limit(20)


def _cases(db: monorm.Database) -> dict[str, Case]:
    def read_pk(i: int) -> Any:
        return Post.objects.get(Post.id == pk_for(i))

    def read_pk_filter(i: int) -> Any:
        return Post.objects.filter(Post.id == pk_for(i))  # the CPU split builds the query; `get` runs the same statement

    def read_n(n: int, step: int) -> Any:
        def build(i: int) -> Any:
            o, e = page(i, n, step)
            return Post.objects.order_by(Post.id)[o:e]

        return build

    def projection(i: int) -> Any:
        o, e = page(i, 1000, 1000)
        return Post.objects.load(Post.id, Post.title).order_by(Post.id)[o:e]

    def filter_ne_null(i: int) -> Any:
        # category_id is nullable: `!=` drops the NULL rows unless or_null() keeps them (Django's exclude() keeps them)
        return Post.objects.filter((Post.category_id != 3).or_null()).order_by(Post.id)[(i % 9) * 20 : (i % 9) * 20 + 20]

    def aggregate(i: int) -> Any:
        return (
            Post.objects.select(Post.author_id, func.sum(Post.views).label("total"))
            .group_by(Post.author_id)
            .order_by(Post.author_id)
        )

    def join(i: int) -> Any:
        o, e = page(i, 50, 50)
        return Post.objects.order_by(Post.id)[o:e].load(Post.author)

    def prefetch(i: int) -> Any:
        o, e = page(i, 50, 50)
        return User.objects.order_by(User.id)[o:e].load(User.posts)

    def m2m(i: int) -> Any:
        o, e = page(i, 50, 50)
        return Post.objects.order_by(Post.id)[o:e].load(Post.tags)

    async def run(q: Any) -> Any:
        return await q

    async def count(i: int) -> int:
        return await Post.objects.filter(Post.published, Post.views > 500).count()

    def sql(q: Any) -> str:
        return str(q.sql())

    def reads(builder: Any, fields: tuple[str, ...], *, cpu: bool = True) -> Case:
        return Case(
            op=lambda i: run(builder(i)),
            norm=lambda raw: objs(raw if isinstance(raw, list) else [raw], fields),
            build=builder if cpu else None,
            sql=(lambda i: sql(builder(i))) if cpu else None,
        )

    def related(builder: Any, fields: tuple[str, ...], rel: str, rel_fields: tuple[str, ...], many: bool) -> Case:
        def norm(raw: list[Any]) -> Any:
            out = []
            for o in raw:
                r = getattr(o, rel)
                items = sorted(r.cached, key=lambda x: x.id) if many else [r]
                out.append([objs([o], fields)[0], objs(items, rel_fields)])
            return out

        return Case(op=lambda i: run(builder(i)), norm=norm, build=builder, sql=lambda i: sql(builder(i)))

    async def insert_1(rows: list[dict[str, Any]]) -> Any:
        return await Comment.objects.insert(**rows[0])

    async def bulk_insert(rows: list[dict[str, Any]]) -> Any:
        return await Comment.objects.insert_many(rows).returning()

    async def update_1(i: int) -> int:
        return await Post.objects.filter(Post.id == pk_for(i)).update(views=Post.views + 1)

    async def bulk_update(i: int) -> int:
        lo, hi = update_range(i)
        return await Post.objects.filter(Post.id.between(lo, hi)).update(views=Post.views + 1)

    async def upsert(rows: list[dict[str, Any]]) -> Any:
        return await User.objects.insert_many(rows).on_conflict(User.email, update=True, update_fields=[User.name])

    async def delete_100(i: int) -> int:
        return await Comment.objects.filter(Comment.post_id == 1, Comment.body == "del-bench").delete()

    async def tx_5(i: int) -> Any:
        p = pk_for(i)
        rows = comment_rows(i, 2)
        async with db.transaction():
            first = await Comment.objects.insert(**rows[0])
            await Comment.objects.insert(**rows[1])
            await Post.objects.filter(Post.id == p).update(views=Post.views + 1)
            post = await Post.objects.get(Post.id == p)
            await Comment.objects.filter(Comment.id == first.id).delete()
        return post

    async def one_task(t: int, i: int) -> tuple[list[Any], list[float]]:
        out, lat = [], []
        for k in range(CONCURRENT_OPS_PER_TASK):
            n = t * CONCURRENT_OPS_PER_TASK + k
            t0 = time.perf_counter()
            if k % 2 == 0:
                out.append(objs([await Post.objects.get(Post.id == pk_for(n + i))], POST))
            else:
                out.append(objs(await _filter(n % 900), POST))
            lat.append((time.perf_counter() - t0) * 1000)
        return out, lat

    async def concurrent(i: int) -> Any:
        return await asyncio.gather(*(one_task(t, i) for t in range(CONCURRENT_TASKS)))

    def concurrent_norm(raw: Any) -> Any:
        return [r[0] for r in raw]

    return {
        "read-pk": Case(
            op=lambda i: run(read_pk(i)),
            norm=lambda raw: objs([raw], POST),
            build=read_pk_filter,
            sql=lambda i: sql(read_pk_filter(i)),
        ),
        "read-50": reads(read_n(50, 50), POST),
        "read-1000": reads(read_n(1000, 1000), POST),
        "projection-1000": Case(
            op=lambda i: run(projection(i)),
            norm=lambda raw: [[o.id, o.title] for o in raw],
            build=projection,
            sql=lambda i: sql(projection(i)),
        ),
        "filter": reads(lambda i: _filter(), POST),
        "filter-ne-null": reads(filter_ne_null, POST),
        "count": Case(op=count, norm=lambda raw: raw),
        "aggregate": Case(
            op=lambda i: run(aggregate(i)),
            norm=lambda raw: [[plain(r[0]), int(r[1])] for r in raw],
            build=aggregate,
            sql=lambda i: sql(aggregate(i)),
        ),
        "join-to-one": related(join, POST, "author", USER, many=False),
        "prefetch-to-many": related(prefetch, USER, "posts", POST, many=True),
        "m2m": related(m2m, POST, "tags", TAG, many=True),
        "insert-1": Case(op=insert_1, setup=lambda i, c: comment_rows(i, 1), norm=lambda raw: raw.id),
        "bulk-insert-100": Case(
            op=bulk_insert, setup=lambda i, c: comment_rows(i, 100), norm=lambda raw: [o.id for o in raw]
        ),
        "bulk-insert-1000": Case(
            op=bulk_insert, setup=lambda i, c: comment_rows(i, 1000), norm=lambda raw: [o.id for o in raw]
        ),
        "update-1": Case(op=update_1, norm=lambda raw: raw),
        "bulk-update-100": Case(op=bulk_update, norm=lambda raw: raw),
        "upsert-100": Case(op=upsert, setup=lambda i, c: upsert_rows(i), norm=lambda raw: raw),
        "delete-100": Case(op=delete_100, setup=delete_setup, norm=lambda raw: raw),
        "tx-5": Case(op=tx_5, norm=lambda raw: objs([raw], POST)),
        "concurrent": Case(op=concurrent, norm=concurrent_norm, concurrent=True),
    }
