"""Django ORM, sync and async, written as the Django docs show."""

from __future__ import annotations

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
    objs,
    pk_for,
    plain,
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


def _page(i: int, size: int, step: int) -> tuple[int, int]:
    o = (i % 9) * step
    return o, o + size


def _delete_setup(i: int, conn: Any) -> int:
    conn.execute(
        "INSERT INTO comments (post_id, author_id, body, created_at) "
        "SELECT 1, NULL, 'del-bench', now() FROM generate_series(1, 100)"
    )
    return i


def _flt(threshold: int = 500) -> Any:
    return M.Post.objects.filter(published=True, views__gt=threshold).order_by("-views", "id")[:20]


class DjangoContender(Contender):
    """`asynchronous` picks the async ORM (`aget`, `async for`); else the sync ORM."""

    def __init__(self, asynchronous: bool) -> None:
        self.asynchronous = asynchronous
        self.name = "django-async" if asynchronous else "django-sync"

    async def start(self) -> None:
        _setup()
        self.cases = self._async_cases() if self.asynchronous else self._sync_cases()

    async def stop(self) -> None:
        from django.db import connections

        if self.asynchronous:
            await sync_to_async(connections.close_all)()
        else:
            connections.close_all()

    def info(self) -> dict[str, Any]:
        import django
        from django.db import connection

        return {
            "django": django.get_version(),
            "prepared_statements": "psycopg default: server-side prepare after 5 executions of one statement",
            "pool": f"psycopg_pool min=max={POOL_SIZE}, one pool per process, connection held per thread until closed",
            "async_note": "Django async ORM runs each call through sync_to_async (thread-sensitive): calls serialize on one thread",
            "vendor": connection.vendor,
        }

    # -- sync ---------------------------------------------------------------------------------
    def _sync_cases(self) -> dict[str, Case]:
        from django.db import transaction
        from django.db.models import F, Sum

        P, U, C = M.Post, M.User, M.Comment

        def reads(builder: Any, fields: tuple[str, ...]) -> Case:
            return Case(
                op=lambda i: list(builder(i)),
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

            return Case(
                op=lambda i: list(builder(i)), norm=norm, build=builder, sql=lambda i: str(builder(i).query)
            )

        def page(qs: Any, n: int, step: int, i: int) -> Any:
            o, e = _page(i, n, step)
            return qs.order_by("id")[o:e]

        def aggregate(i: int) -> Any:
            return P.objects.values("author_id").annotate(total=Sum("views")).order_by("author_id")

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
                connection.close()  # give the pooled connection back
            return out, lat

        def concurrent(i: int) -> Any:
            with ThreadPoolExecutor(CONCURRENT_TASKS) as ex:
                return list(ex.map(lambda t: concurrent_task(t, i), range(CONCURRENT_TASKS)))

        def tx_5(i: int) -> Any:
            p = pk_for(i)
            rows = comment_rows(i, 2)
            with transaction.atomic():
                first = C.objects.create(**rows[0])
                C.objects.create(**rows[1])
                P.objects.filter(id=p).update(views=F("views") + 1)
                post = P.objects.get(id=p)
                C.objects.filter(id=first.id).delete()
            return post

        return {
            "read-pk": Case(
                op=lambda i: P.objects.get(id=pk_for(i)),
                norm=lambda raw: objs([raw], POST),
                build=lambda i: P.objects.filter(id=pk_for(i)),
                sql=lambda i: str(P.objects.filter(id=pk_for(i)).query),
            ),
            "read-50": reads(lambda i: page(P.objects.all(), 50, 50, i), POST),
            "read-1000": reads(lambda i: page(P.objects.all(), 1000, 1000, i), POST),
            "projection-1000": Case(
                op=lambda i: list(page(P.objects.only("id", "title"), 1000, 1000, i)),
                norm=lambda raw: [[o.id, o.title] for o in raw],
                build=lambda i: page(P.objects.only("id", "title"), 1000, 1000, i),
                sql=lambda i: str(page(P.objects.only("id", "title"), 1000, 1000, i).query),
            ),
            "filter": reads(lambda i: _flt(), POST),
            "filter-ne-null": reads(
                lambda i: P.objects.exclude(category_id=3).order_by("id")[(i % 9) * 20 : (i % 9) * 20 + 20], POST
            ),
            "count": Case(
                op=lambda i: P.objects.filter(published=True, views__gt=500).count(), norm=lambda raw: raw
            ),
            "aggregate": Case(
                op=lambda i: list(aggregate(i)),
                norm=lambda raw: [[r["author_id"], int(r["total"])] for r in raw],
                build=aggregate,
                sql=lambda i: str(aggregate(i).query),
            ),
            "join-to-one": related(
                lambda i: page(P.objects.select_related("author"), 50, 50, i), POST, "author", USER, many=False
            ),
            "prefetch-to-many": related(
                lambda i: U.objects.prefetch_related("posts").order_by("id")[_page(i, 50, 50)[0] : _page(i, 50, 50)[1]],
                USER,
                "posts",
                POST,
                many=True,
            ),
            "m2m": related(
                lambda i: page(P.objects.prefetch_related("tags"), 50, 50, i), POST, "tags", TAG, many=True
            ),
            "insert-1": Case(
                op=lambda rows: C.objects.create(**rows[0]), setup=lambda i, c: comment_rows(i, 1), norm=lambda r: r.id
            ),
            "bulk-insert-100": Case(
                op=lambda rows: C.objects.bulk_create([C(**r) for r in rows]),
                setup=lambda i, c: comment_rows(i, 100),
                norm=lambda raw: [o.id for o in raw],
            ),
            "bulk-insert-1000": Case(
                op=lambda rows: C.objects.bulk_create([C(**r) for r in rows]),
                setup=lambda i, c: comment_rows(i, 1000),
                norm=lambda raw: [o.id for o in raw],
            ),
            "update-1": Case(
                op=lambda i: P.objects.filter(id=pk_for(i)).update(views=F("views") + 1), norm=lambda raw: raw
            ),
            "bulk-update-100": Case(
                op=lambda i: P.objects.filter(id__range=update_range(i)).update(views=F("views") + 1),
                norm=lambda raw: raw,
            ),
            "upsert-100": Case(
                op=lambda rows: U.objects.bulk_create(
                    [U(**r) for r in rows], update_conflicts=True, unique_fields=["email"], update_fields=["name"]
                ),
                setup=lambda i, c: upsert_rows(i),
                norm=lambda raw: len(raw),
            ),
            "delete-100": Case(
                op=lambda i: C.objects.filter(post_id=1, body="del-bench").delete()[0],
                setup=_delete_setup,
                norm=lambda raw: raw,
            ),
            "tx-5": Case(op=tx_5, norm=lambda raw: objs([raw], POST)),
            "concurrent": Case(op=concurrent, norm=lambda raw: [r[0] for r in raw], concurrent=True),
        }

    # -- async --------------------------------------------------------------------------------
    def _async_cases(self) -> dict[str, Case]:
        from django.db import transaction
        from django.db.models import F, Sum

        P, U, C = M.Post, M.User, M.Comment

        async def alist(qs: Any) -> list[Any]:
            return [o async for o in qs]

        def page(qs: Any, n: int, step: int, i: int) -> Any:
            o, e = _page(i, n, step)
            return qs.order_by("id")[o:e]

        def reads(builder: Any, fields: tuple[str, ...]) -> Case:
            return Case(
                op=lambda i: alist(builder(i)),
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

            return Case(op=lambda i: alist(builder(i)), norm=norm, build=builder, sql=lambda i: str(builder(i).query))

        def aggregate(i: int) -> Any:
            return P.objects.values("author_id").annotate(total=Sum("views")).order_by("author_id")

        async def concurrent_task(t: int, i: int) -> tuple[list[Any], list[float]]:
            out, lat = [], []
            for k in range(CONCURRENT_OPS_PER_TASK):
                n = t * CONCURRENT_OPS_PER_TASK + k
                t0 = time.perf_counter()
                if k % 2 == 0:
                    out.append(objs([await P.objects.aget(id=pk_for(n + i))], POST))
                else:
                    out.append(objs(await alist(_flt(n % 900)), POST))
                lat.append((time.perf_counter() - t0) * 1000)
            return out, lat

        async def concurrent(i: int) -> Any:
            import asyncio

            return await asyncio.gather(*(concurrent_task(t, i) for t in range(CONCURRENT_TASKS)))

        def tx_body(i: int) -> Any:
            p = pk_for(i)
            rows = comment_rows(i, 2)
            with transaction.atomic():
                first = C.objects.create(**rows[0])
                C.objects.create(**rows[1])
                P.objects.filter(id=p).update(views=F("views") + 1)
                post = P.objects.get(id=p)
                C.objects.filter(id=first.id).delete()
            return post

        # Django has no async atomic(): the documented way is a sync function wrapped in sync_to_async.
        tx_5 = sync_to_async(tx_body)

        async def upsert(rows: list[dict[str, Any]]) -> Any:
            return await U.objects.abulk_create(
                [U(**r) for r in rows], update_conflicts=True, unique_fields=["email"], update_fields=["name"]
            )

        async def delete_100(i: int) -> int:
            return (await C.objects.filter(post_id=1, body="del-bench").adelete())[0]

        return {
            "read-pk": Case(
                op=lambda i: P.objects.aget(id=pk_for(i)),
                norm=lambda raw: objs([raw], POST),
                build=lambda i: P.objects.filter(id=pk_for(i)),
                sql=lambda i: str(P.objects.filter(id=pk_for(i)).query),
            ),
            "read-50": reads(lambda i: page(P.objects.all(), 50, 50, i), POST),
            "read-1000": reads(lambda i: page(P.objects.all(), 1000, 1000, i), POST),
            "projection-1000": Case(
                op=lambda i: alist(page(P.objects.only("id", "title"), 1000, 1000, i)),
                norm=lambda raw: [[o.id, o.title] for o in raw],
                build=lambda i: page(P.objects.only("id", "title"), 1000, 1000, i),
                sql=lambda i: str(page(P.objects.only("id", "title"), 1000, 1000, i).query),
            ),
            "filter": reads(lambda i: _flt(), POST),
            "filter-ne-null": reads(
                lambda i: P.objects.exclude(category_id=3).order_by("id")[(i % 9) * 20 : (i % 9) * 20 + 20], POST
            ),
            "count": Case(op=lambda i: P.objects.filter(published=True, views__gt=500).acount(), norm=lambda raw: raw),
            "aggregate": Case(
                op=lambda i: alist(aggregate(i)),
                norm=lambda raw: [[r["author_id"], int(r["total"])] for r in raw],
                build=aggregate,
                sql=lambda i: str(aggregate(i).query),
            ),
            "join-to-one": related(
                lambda i: page(P.objects.select_related("author"), 50, 50, i), POST, "author", USER, many=False
            ),
            "prefetch-to-many": related(
                lambda i: U.objects.prefetch_related("posts").order_by("id")[_page(i, 50, 50)[0] : _page(i, 50, 50)[1]],
                USER,
                "posts",
                POST,
                many=True,
            ),
            "m2m": related(lambda i: page(P.objects.prefetch_related("tags"), 50, 50, i), POST, "tags", TAG, many=True),
            "insert-1": Case(
                op=lambda rows: C.objects.acreate(**rows[0]), setup=lambda i, c: comment_rows(i, 1), norm=lambda r: r.id
            ),
            "bulk-insert-100": Case(
                op=lambda rows: C.objects.abulk_create([C(**r) for r in rows]),
                setup=lambda i, c: comment_rows(i, 100),
                norm=lambda raw: [o.id for o in raw],
            ),
            "bulk-insert-1000": Case(
                op=lambda rows: C.objects.abulk_create([C(**r) for r in rows]),
                setup=lambda i, c: comment_rows(i, 1000),
                norm=lambda raw: [o.id for o in raw],
            ),
            "update-1": Case(
                op=lambda i: P.objects.filter(id=pk_for(i)).aupdate(views=F("views") + 1), norm=lambda raw: raw
            ),
            "bulk-update-100": Case(
                op=lambda i: P.objects.filter(id__range=update_range(i)).aupdate(views=F("views") + 1),
                norm=lambda raw: raw,
            ),
            "upsert-100": Case(op=upsert, setup=lambda i, c: upsert_rows(i), norm=lambda raw: len(raw)),
            "delete-100": Case(op=delete_100, setup=_delete_setup, norm=lambda raw: raw),
            "tx-5": Case(op=tx_5, norm=lambda raw: objs([raw], POST)),
            "concurrent": Case(op=concurrent, norm=lambda raw: [r[0] for r in raw], concurrent=True),
        }
