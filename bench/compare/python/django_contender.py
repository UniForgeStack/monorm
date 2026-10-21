"""Django ORM, sync and async, written as the Django docs show."""

from __future__ import annotations

import asyncio
import os
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from asgiref.sync import sync_to_async
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
    update_range,
    upsert_rows,
)
from contender import POST, TAG, USER, Case, Contender

M: Any = None  # the `blog.models` module, after django.setup()


def _setup() -> None:
    os.environ["BENCH_DATABASE_URL"] = database_url()
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "benchsite.settings")
    import django

    django.setup()
    global M
    from blog import models

    M = models


def _flt(threshold: int = 500) -> Any:
    return M.Post.objects.filter(published=True, views__gt=threshold).order_by("-views", "id")[:20]


def _paged(qs: Any, n: int, step: int, i: int) -> Any:
    o, e = page(i, n, step)
    return qs.order_by("id")[o:e]


def _aggregate(i: int) -> Any:
    from django.db.models import Sum

    return M.Post.objects.values("author_id").annotate(total=Sum("views")).order_by("author_id")


def _tx_body(i: int) -> Any:
    from django.db import transaction
    from django.db.models import F

    P, C = M.Post, M.Comment
    p = pk_for(i)
    rows = comment_rows(i, 2)
    with transaction.atomic():
        first = C.objects.create(**rows[0])
        C.objects.create(**rows[1])
        P.objects.filter(id=p).update(views=F("views") + 1)
        post = P.objects.get(id=p)
        C.objects.filter(id=first.id).delete()
    return post


class _Sync:
    """The sync ORM calls the shared case table needs."""

    @staticmethod
    def fetch(qs: Any) -> Any:
        return list(qs)

    get = staticmethod(lambda qs, **kw: qs.get(**kw))
    count = staticmethod(lambda qs: qs.count())
    create = staticmethod(lambda qs, **kw: qs.create(**kw))
    bulk_create = staticmethod(lambda qs, objs, **kw: qs.bulk_create(objs, **kw))
    update = staticmethod(lambda qs, **kw: qs.update(**kw))
    delete = staticmethod(lambda qs: qs.delete())


class _Async:
    """The async ORM calls (`aget`, `async for`, ...); each returns a coroutine."""

    @staticmethod
    async def fetch(qs: Any) -> Any:
        return [o async for o in qs]

    get = staticmethod(lambda qs, **kw: qs.aget(**kw))
    count = staticmethod(lambda qs: qs.acount())
    create = staticmethod(lambda qs, **kw: qs.acreate(**kw))
    bulk_create = staticmethod(lambda qs, objs, **kw: qs.abulk_create(objs, **kw))
    update = staticmethod(lambda qs, **kw: qs.aupdate(**kw))
    delete = staticmethod(lambda qs: qs.adelete())


class DjangoContender(Contender):
    """`asynchronous` picks the async ORM (`aget`, `async for`); else the sync ORM."""

    def __init__(self, asynchronous: bool) -> None:
        self.asynchronous = asynchronous
        self.name = "django-async" if asynchronous else "django-sync"
        self.executor: ThreadPoolExecutor | None = None

    async def start(self) -> None:
        _setup()
        if not self.asynchronous:
            self.executor = ThreadPoolExecutor(CONCURRENT_TASKS)  # outside the timed op: thread start is not query cost
        self.cases = self._cases(_Async if self.asynchronous else _Sync)

    async def stop(self) -> None:
        from django.db import connections

        if self.executor:
            self.executor.shutdown()
        if self.asynchronous:
            await sync_to_async(connections.close_all)()
        else:
            connections.close_all()

    def info(self) -> dict[str, Any]:
        import django
        from django.db import connection

        return {
            "django": django.get_version(),
            "prepared_statements": "psycopg default: server-side prepare after 5 executions of one statement; "
            "Django inlines LIMIT/OFFSET, so the read-N statements differ per page and are not prepared",
            "pool": f"psycopg_pool min=max={POOL_SIZE}, one pool per process; each concurrent task returns its connection with close()",
            "async_note": "Django async ORM runs each call through sync_to_async (thread-sensitive): calls serialize on one thread",
            "delete_note": "QuerySet.delete() runs BEGIN, DELETE, COMMIT: 3 statements",
            "upsert_note": "bulk_create(update_conflicts=True) adds RETURNING id",
            "vendor": connection.vendor,
        }

    def _cases(self, A: Any) -> dict[str, Case]:
        from django.db.models import F

        P, U, C = M.Post, M.User, M.Comment
        tx_async = sync_to_async(_tx_body)

        def pk_filter(i: int) -> Any:
            return P.objects.filter(id=pk_for(i))  # the CPU split builds the query; `get` would run it

        def reads(builder: Any, fields: tuple[str, ...]) -> Case:
            return Case(
                op=lambda i: A.fetch(builder(i)),
                norm=lambda raw: objs(raw, fields),
                build=builder,
                sql=lambda i: str(builder(i).query),
            )

        def related(builder: Any, fields: tuple[str, ...], rel: str, rel_fields: tuple[str, ...], many: bool) -> Case:
            def norm(raw: list[Any]) -> Any:
                out = []
                for o in raw:
                    r = getattr(o, rel)
                    items = sorted(r.all(), key=lambda x: x.id) if many else [r]
                    out.append([objs([o], fields)[0], objs(items, rel_fields)])
                return out

            return Case(op=lambda i: A.fetch(builder(i)), norm=norm, build=builder, sql=lambda i: str(builder(i).query))

        def only_id_title(i: int) -> Any:
            return _paged(P.objects.only("id", "title"), 1000, 1000, i)

        def filter_ne_null(i: int) -> Any:
            return P.objects.exclude(category_id=3).order_by("id")[(i % 9) * 20 : (i % 9) * 20 + 20]

        def prefetch(i: int) -> Any:
            return _paged(U.objects.prefetch_related("posts"), 50, 50, i)

        def bulk(n: int) -> Case:
            return Case(
                op=lambda rows: A.bulk_create(C.objects, [C(**r) for r in rows]),
                setup=lambda i, c: comment_rows(i, n),
                norm=lambda raw: [o.id for o in raw],
            )

        def upsert(rows: list[dict[str, Any]]) -> Any:
            return A.bulk_create(
                U.objects, [U(**r) for r in rows], update_conflicts=True, unique_fields=["email"], update_fields=["name"]
            )

        async def deleted(coro: Any) -> int:
            return (await coro)[0]

        def delete_100(i: int) -> Any:
            r = A.delete(C.objects.filter(post_id=1, body="del-bench"))
            return deleted(r) if self.asynchronous else r[0]

        def tx_5(i: int) -> Any:
            return tx_async(i) if self.asynchronous else _tx_body(i)  # Django has no async atomic()

        def concurrent_task(t: int, i: int) -> tuple[list[Any], list[float]]:
            from django.db import connection

            out, lat = [], []
            try:
                for k in range(CONCURRENT_OPS_PER_TASK):
                    n = t * CONCURRENT_OPS_PER_TASK + k
                    t0 = time.perf_counter()
                    if k % 2 == 0:
                        out.append(objs([P.objects.get(id=pk_for(n + i))], POST))
                    else:
                        out.append(objs(list(_flt(n % 900)), POST))
                    lat.append((time.perf_counter() - t0) * 1000)
            finally:
                connection.close()  # gives the pooled connection back (putconn); it does not disconnect
            return out, lat

        async def concurrent_task_async(t: int, i: int) -> tuple[list[Any], list[float]]:
            out, lat = [], []
            for k in range(CONCURRENT_OPS_PER_TASK):
                n = t * CONCURRENT_OPS_PER_TASK + k
                t0 = time.perf_counter()
                if k % 2 == 0:
                    out.append(objs([await P.objects.aget(id=pk_for(n + i))], POST))
                else:
                    out.append(objs(await _Async.fetch(_flt(n % 900)), POST))
                lat.append((time.perf_counter() - t0) * 1000)
            return out, lat

        def concurrent(i: int) -> Any:
            if self.asynchronous:
                return asyncio.gather(*(concurrent_task_async(t, i) for t in range(CONCURRENT_TASKS)))
            assert self.executor
            return list(self.executor.map(lambda t: concurrent_task(t, i), range(CONCURRENT_TASKS)))

        def release_main_connection(i: int, conn: Any) -> int:
            # Untimed: the main thread would hold 1 of the POOL_SIZE pooled connections from earlier cases.
            from django.db import connection

            if not self.asynchronous:
                connection.close()
            return i

        return {
            "read-pk": Case(
                op=lambda i: A.get(P.objects, id=pk_for(i)),
                norm=lambda raw: objs([raw], POST),
                build=pk_filter,
                sql=lambda i: str(pk_filter(i).query),
            ),
            "read-50": reads(lambda i: _paged(P.objects.all(), 50, 50, i), POST),
            "read-1000": reads(lambda i: _paged(P.objects.all(), 1000, 1000, i), POST),
            "projection-1000": Case(
                op=lambda i: A.fetch(only_id_title(i)),
                norm=lambda raw: [[o.id, o.title] for o in raw],
                build=only_id_title,
                sql=lambda i: str(only_id_title(i).query),
            ),
            "filter": reads(lambda i: _flt(), POST),
            "filter-ne-null": reads(filter_ne_null, POST),
            "count": Case(op=lambda i: A.count(P.objects.filter(published=True, views__gt=500)), norm=lambda raw: raw),
            "aggregate": Case(
                op=lambda i: A.fetch(_aggregate(i)),
                norm=lambda raw: [[r["author_id"], int(r["total"])] for r in raw],
                build=_aggregate,
                sql=lambda i: str(_aggregate(i).query),
            ),
            "join-to-one": related(
                lambda i: _paged(P.objects.select_related("author"), 50, 50, i), POST, "author", USER, many=False
            ),
            "prefetch-to-many": related(prefetch, USER, "posts", POST, many=True),
            "m2m": related(lambda i: _paged(P.objects.prefetch_related("tags"), 50, 50, i), POST, "tags", TAG, many=True),
            "insert-1": Case(
                op=lambda rows: A.create(C.objects, **rows[0]), setup=lambda i, c: comment_rows(i, 1), norm=lambda r: r.id
            ),
            "bulk-insert-100": bulk(100),
            "bulk-insert-1000": bulk(1000),
            "update-1": Case(op=lambda i: A.update(P.objects.filter(id=pk_for(i)), views=F("views") + 1), norm=lambda raw: raw),
            "bulk-update-100": Case(
                op=lambda i: A.update(P.objects.filter(id__range=update_range(i)), views=F("views") + 1), norm=lambda raw: raw
            ),
            "upsert-100": Case(op=upsert, setup=lambda i, c: upsert_rows(i), norm=lambda raw: len(raw)),
            "delete-100": Case(op=delete_100, setup=delete_setup, norm=lambda raw: raw),
            "tx-5": Case(op=tx_5, norm=lambda raw: objs([raw], POST)),
            "concurrent": Case(
                op=concurrent, setup=release_main_connection, norm=lambda raw: [r[0] for r in raw], concurrent=True
            ),
        }
