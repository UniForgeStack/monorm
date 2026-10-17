"""`db.on_query` hooks, `orm.otel` spans, and the N+1 finder on the same events."""

import asyncio
import contextvars
import subprocess
import sys
import types

import pytest
import orm
from orm import debug
from blog.models import Comment, Post, User

SOURCE = """datasource db {
  provider = "sqlite"
}
model Person {
  id        Int        @id
  name      String     @unique
  customers Customer[]
}
model Customer {
  id        Int    @id
  person_id Int
  person    Person @relation(fields: [person_id], references: [id])
}
"""


@pytest.fixture
async def shop():
    registry = orm.Registry()
    models = orm.loads(SOURCE, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    Person, Customer = models["Person"], models["Customer"]
    await Person.objects.using(db).insert_many([{"id": i, "name": f"p{i}"} for i in range(3)])
    await Customer.objects.using(db).insert_many([{"id": i, "person_id": i % 2} for i in range(4)])
    yield Person, Customer, db
    await db.close()


async def test_a_hook_gets_each_statement_with_its_shape_time_and_rows(shop):
    Person, Customer, db = shop
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    people = await Person.objects.using(db).load(Person.customers).order_by(Person.id)
    assert [len(p.customers.cached) for p in people] == [2, 2, 0]
    select, prefetch = events
    assert select.sql.startswith("SELECT") and '"person"' in select.sql and select.rows == 3
    assert '"customer"' in prefetch.sql and "?" in prefetch.sql and prefetch.rows == 4
    assert select.error is None and 0 <= select.duration < 5 and select.start <= prefetch.start

    events.clear()
    assert (
        await Customer.objects.using(db).update_many([{"id": i, "person_id": 2} for i in range(3)], batch_size=1) == 3
    )
    assert [e.rows for e in events] == [1, 1, 1] and all(e.sql.startswith("UPDATE") for e in events)

    events.clear()
    assert await db.execute("DELETE FROM customer WHERE id = 3") == 1
    assert [(e.sql, e.rows) for e in events] == [("DELETE FROM customer WHERE id = 3", 1)]

    remove()
    events.clear()
    await Person.objects.using(db).count()
    assert events == []


async def test_a_failed_statement_gives_an_event_with_the_error(shop):
    Person, _, db = shop
    events: list[orm.QueryEvent] = []
    db.on_query(events.append)
    with pytest.raises(orm.IntegrityError):
        await Person.objects.using(db).insert(id=9, name="p0")
    [e] = events
    assert e.sql.startswith("INSERT INTO") and e.rows == 0 and e.error is not None and "UNIQUE" in e.error


async def test_hooks_run_in_the_callers_context_and_see_prepared_queries(shop):
    Person, _, db = shop
    request = contextvars.ContextVar("request", default="-")
    seen: list[tuple[str, int]] = []
    db.on_query(lambda e: seen.append((request.get(), e.rows)))
    by_name = Person.objects.using(db).filter(Person.name == orm.param("name")).prepare()
    request.set("r1")
    assert (await by_name.get(name="p1")).id == 1
    async with db.transaction():
        request.set("r2")
        await Person.objects.using(db).filter(Person.id == 1).update(name="x")
    assert seen == [("r1", 1), ("r2", 1)]


async def test_a_hook_error_reaches_the_caller(shop):
    Person, _, db = shop

    def broken(e: orm.QueryEvent) -> None:
        raise RuntimeError("exporter down")

    db.on_query(broken)
    with pytest.raises(RuntimeError, match="exporter down"):
        await Person.objects.using(db).count()


async def test_an_async_hook_is_awaited_and_its_error_reaches_the_caller(shop):
    Person, _, db = shop
    seen: list[str] = []

    async def record(e: orm.QueryEvent) -> None:
        await asyncio.sleep(0)
        seen.append(e.sql)

    remove = db.on_query(record)
    await Person.objects.using(db).count()
    assert len(seen) == 1
    remove()

    async def broken(e: orm.QueryEvent) -> None:
        await asyncio.sleep(0)
        raise RuntimeError("async exporter down")

    db.on_query(broken)
    with pytest.raises(RuntimeError, match="async exporter down"):
        await Person.objects.using(db).count()


async def test_a_hook_error_does_not_replace_the_error_of_the_call(shop):
    Person, _, db = shop
    later: list[orm.QueryEvent] = []

    def broken(e: orm.QueryEvent) -> None:
        raise RuntimeError("exporter down")

    db.on_query(broken)
    db.on_query(later.append)
    with pytest.raises(orm.IntegrityError) as caught:
        await Person.objects.using(db).insert(id=9, name="p0")
    assert [e.error is not None for e in later] == [True]
    assert any("exporter down" in note for note in caught.value.__notes__)


async def test_every_hook_gets_every_event_before_the_first_hook_error_is_raised(shop):
    Person, _, db = shop
    calls: list[str] = []

    def failing(name: str):
        def hook(e: orm.QueryEvent) -> None:
            calls.append(name)
            raise RuntimeError(name)

        return hook

    db.on_query(failing("first"))
    db.on_query(failing("second"))
    with pytest.raises(RuntimeError, match="first") as caught:
        await Person.objects.using(db).load(Person.customers)  # two statements
    assert calls == ["first", "first", "second", "second"]
    assert len(caught.value.__notes__) == 3


async def test_the_statements_of_a_hook_give_no_events_and_no_n_plus_one_count(shop):
    Person, _, db = shop
    await db.execute("CREATE TABLE audit (sql TEXT)")
    calls: list[str] = []

    async def audit(e: orm.QueryEvent) -> None:
        calls.append(e.sql)
        if len(calls) > 5:  # a hook that sees its own statements would never end
            return
        for _ in range(3):
            await db.fetch("SELECT count(*) AS n FROM audit")
        await db.execute("INSERT INTO audit VALUES ('x')")

    remove = db.on_query(audit)
    async with asyncio.timeout(10):
        with debug.n_plus_one(threshold=2) as report:
            await Person.objects.using(db).count()
    remove()
    assert len(calls) == 1
    assert len(report.shapes) == 1 and not report.repeated
    assert await db.fetch("SELECT count(*) AS n FROM audit") == [{"n": 1}]


async def test_remove_takes_out_its_own_registration_once(shop):
    Person, _, db = shop
    seen: list[orm.QueryEvent] = []
    remove_first = db.on_query(seen.append)
    db.on_query(seen.append)
    remove_first()
    remove_first()
    await Person.objects.using(db).count()
    assert len(seen) == 1


async def test_a_hook_that_changes_the_hooks_does_not_change_the_call_in_progress(shop):
    Person, _, db = shop
    seen: list[str] = []

    def once(e: orm.QueryEvent) -> None:
        seen.append("once")
        remove_once()
        db.on_query(lambda e: seen.append("late"))

    remove_once = db.on_query(once)
    db.on_query(lambda e: seen.append("always"))
    await Person.objects.using(db).count()
    assert seen == ["once", "always"]
    await Person.objects.using(db).count()
    assert seen == ["once", "always", "always", "late"]


def test_orm_otel_is_a_member_of_the_package():
    done = subprocess.run([sys.executable, "-c", "import orm; orm.otel.instrument"], capture_output=True, text=True)
    assert done.returncode == 0, done.stderr


class FakeSpan:
    def __init__(self, name, kind, attributes, start_time):
        self.name, self.kind, self.attributes, self.start = name, kind, attributes, start_time
        self.status = None
        self.end_time = None

    def set_status(self, code, description=None):
        self.status = (code, description)

    def end(self, end_time=None):
        self.end_time = end_time


class FakeTracer:
    def __init__(self):
        self.spans: list[FakeSpan] = []

    def start_span(self, name, *, kind=None, attributes=None, start_time=None):
        self.spans.append(FakeSpan(name, kind, attributes, start_time))
        return self.spans[-1]


@pytest.fixture
def opentelemetry(monkeypatch):
    """A stand-in for the `opentelemetry.trace` module: the suite runs without the package."""
    trace = types.ModuleType("opentelemetry.trace")
    trace.SpanKind = types.SimpleNamespace(CLIENT="client")
    trace.StatusCode = types.SimpleNamespace(ERROR="error")
    trace.tracer = FakeTracer()
    trace.get_tracer = lambda name: trace.tracer
    package = types.ModuleType("opentelemetry")
    package.trace = trace
    monkeypatch.setitem(sys.modules, "opentelemetry", package)
    monkeypatch.setitem(sys.modules, "opentelemetry.trace", trace)
    return trace


async def test_otel_gives_a_client_span_per_statement(shop, opentelemetry):
    from orm import otel

    Person, _, db = shop
    stop = otel.instrument(db)
    await Person.objects.using(db).filter(Person.id == 1)
    with pytest.raises(orm.IntegrityError):
        await Person.objects.using(db).insert(id=9, name="p0")
    ok, failed = opentelemetry.tracer.spans
    assert (ok.name, ok.kind, ok.status) == ("SELECT", "client", None)
    assert ok.attributes["db.system.name"] == "sqlite" and ok.attributes["db.response.returned_rows"] == 1
    assert ok.attributes["db.query.text"].startswith("SELECT") and "?" in ok.attributes["db.query.text"]
    assert ok.start > 1_600_000_000 * 10**9 and ok.end_time >= ok.start
    assert failed.name == "INSERT" and failed.status[0] == "error" and "UNIQUE" in failed.status[1]
    stop()
    await Person.objects.using(db).count()
    assert len(opentelemetry.tracer.spans) == 2

    mine = FakeTracer()
    otel.instrument(db, tracer=mine)
    await Person.objects.using(db).count()
    assert [s.name for s in mine.spans] == ["SELECT"]


async def test_otel_names_a_statement_that_has_no_word(shop, opentelemetry):
    from orm import otel

    _, _, db = shop
    otel.instrument(db)
    with pytest.raises(orm.DatabaseError):
        await db.execute("(")
    [span] = opentelemetry.tracer.spans
    assert span.name == "SQL" and span.status[0] == "error"


def test_otel_without_the_package_names_it(monkeypatch):
    from orm import otel

    monkeypatch.setitem(sys.modules, "opentelemetry", None)
    with pytest.raises(ImportError, match="opentelemetry-api"):
        otel.instrument(object())  # type: ignore[arg-type]


async def test_the_n_plus_one_finder_counts_the_traced_sql(shop):
    Person, Customer, db = shop
    with pytest.warns(debug.NPlusOneWarning), debug.n_plus_one(threshold=2) as report:
        for i in range(3):
            line = sys._getframe().f_lineno + 1
            await Person.objects.using(db).insert(id=10 + i, name=f"n{i}")
    [shape] = report.repeated
    assert shape.count == 3 and shape.sql.startswith('INSERT INTO "person"') and "?" in shape.sql
    assert shape.site.endswith(f"test_query_hooks.py:{line}")


async def test_postgres_events_and_row_counts(clean):
    db = orm.get_database()
    user = await User.objects.insert(email="hook@example.com", name="H")
    post = await Post.objects.insert(author_id=user.id, title="t", body="b")
    await Comment.objects.insert_many([{"post_id": post.id, "body": str(i)} for i in range(3)])
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    try:
        await Post.objects.load(Post.comments).filter(Post.id == post.id)
        assert await Comment.objects.filter(Comment.post_id == post.id).update(body="x") == 3
    finally:
        remove()
    assert [e.rows for e in events] == [1, 3, 3]
    assert events[0].sql.endswith('WHERE "posts"."id" = $1')


async def test_postgres_a_cancelled_statement_gives_an_event_with_the_error_cancelled(clean):
    db = orm.get_database()
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    try:
        with pytest.raises(TimeoutError):
            async with asyncio.timeout(0.5):
                await db.fetch("SELECT pg_sleep(1.5)::text")
    finally:
        remove()
    [e] = events
    assert (e.sql, e.rows, e.error) == ("SELECT pg_sleep(1.5)::text", 0, "cancelled") and 0.3 < e.duration < 1.5


async def test_postgres_the_error_of_an_event_has_the_first_line_only(clean):
    db = orm.get_database()
    await User.objects.insert(email="dup@example.com", name="A")
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    try:
        with pytest.raises(orm.IntegrityError) as caught:
            await User.objects.insert(email="dup@example.com", name="B")
    finally:
        remove()
    [e] = events
    assert e.error is not None and "\n" not in e.error and "dup@example.com" not in e.error
    assert "dup@example.com" in str(caught.value)


async def test_postgres_a_failed_nowait_lock_reports_0_rows_and_a_won_lock_1(clean):
    db = orm.get_database()
    other = await orm.connect(db.url, default=False)
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    try:
        async with other.transaction():
            await other.lock(4242)
            async with db.transaction():
                assert await db.lock(4242, nowait=True) is False
                await other.execute("SELECT 1")  # keeps `other` busy: the lock is still held
        async with db.transaction():
            assert await db.lock(4242, nowait=True) is True
    finally:
        remove()
        await other.close()
    assert [e.rows for e in events if "advisory" in e.sql] == [0, 1]


async def test_otel_cuts_a_long_statement_at_4096_characters(shop, opentelemetry):
    _, _, db = shop
    spans = FakeTracer()
    stop = orm.otel.instrument(db, tracer=spans, raw_sql=True)
    try:
        await db.execute("SELECT 1 /* " + "x" * 5000 + " */")
        await db.execute("SELECT 2")
    finally:
        stop()
    long, short = (s.attributes["db.query.text"] for s in spans.spans)
    assert len(long) == 4099 and long.endswith("...") and short == "SELECT 2"


async def test_raw_sql_without_parameters_has_the_raw_flag_and_no_span_text_by_default(shop, opentelemetry):
    Person, _, db = shop
    events: list[orm.QueryEvent] = []
    db.on_query(events.append)
    spans = FakeTracer()
    stop = orm.otel.instrument(db, tracer=spans)
    await db.execute("UPDATE person SET name = 'secret-literal' WHERE id = 1")
    await db.fetch("SELECT name FROM person WHERE id = ?", 1)
    await Person.objects.using(db).filter(Person.id == 1)
    stop()
    assert [e.raw for e in events] == [True, False, False]
    assert "secret-literal" in events[0].sql
    execute, fetch, built = spans.spans
    assert "db.query.text" not in execute.attributes and execute.name == "UPDATE"
    assert fetch.attributes["db.query.text"].startswith("SELECT name") and built.attributes["db.query.text"]

    stop = orm.otel.instrument(db, tracer=(opted := FakeTracer()), raw_sql=True)
    await db.execute("UPDATE person SET name = 'secret-literal' WHERE id = 1")
    stop()
    assert "secret-literal" in opted.spans[0].attributes["db.query.text"]


async def test_postgres_raw_sql_has_the_raw_flag(clean):
    events: list[orm.QueryEvent] = []
    clean.on_query(events.append)
    await clean.execute("SELECT 'secret-literal'")
    await clean.fetch("SELECT $1::text", "x")
    assert [e.raw for e in events] == [True, False]
