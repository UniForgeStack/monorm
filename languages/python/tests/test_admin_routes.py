"""Custom admin routes (`Admin.route`): the session, the permission, the request, the JSON and stream answers,
through `admin.test_client` on PostgreSQL and SQLite."""

import os

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SECRET = "s" * 40
BASE = "/admin/api/routes"
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA % request.param)
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry, model_methods={"User": UserMethods})
    monorm.use_query_set(models["User"], UserQueries)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError as e:
        pytest.skip(f"{url} not reachable: {e}")
    await db.drop_tables()
    await db.create_tables()
    pw = make_password("pw")
    await models["User"].objects.using(db).insert_many(
        [{"username": name, "email": f"{name}@example.com", "password": pw, "is_superuser": name == "root"} for name in ("ann", "bob", "cy", "root")]
    )
    yield registry, db, models
    await db.drop_tables()
    await db.close()


async def user_id(db, models, name):
    User = models["User"]
    return (await User.objects.using(db).filter(User.username == name).first()).id


async def grant(db, models, username, names):
    perms = models["Permission"]
    rows = [r for r in await perms.objects.using(db) if f"{r.label}.{r.codename}" in names]
    assert len(rows) == len(names)
    Role = models["Role"]
    await Role.objects.using(db).insert_many([{"name": f"for-{username}"}])
    role = await Role.objects.using(db).filter(Role.name == f"for-{username}").first()
    uid = await user_id(db, models, username)
    await models["RolePermission"].objects.using(db).insert_many([{"role_id": role.id, "permission_id": p.id} for p in rows])
    await models["UserRole"].objects.using(db).insert_many([{"user_id": uid, "role_id": role.id}])
    return uid


async def audit(db, models):
    entry = models["AdminLogEntry"]
    return [(r.action, r.resource, r.changes) for r in await entry.objects.using(db).order_by(entry.id)]


async def summary(ctx, request):
    return {"rows": 3}


async def report(ctx, request):
    return {"id": request.params["id"], "user": ctx.user["key"], "has_db": ctx.database is not None, "request_id": isinstance(ctx.request_id, str)}


async def build(world, *routes):
    registry, db, models = world
    site = admin.Admin(registry)
    site.register(models["Role"])
    for method, path, fn, permission in routes:
        site.route(method, path, fn, permission=permission)
    await site.sync_permissions(db)
    return site, db, models


async def test_a_route_without_a_session_is_401_and_the_function_does_not_run(world):
    calls = []

    async def fn(ctx, request):
        calls.append(request.path)

    site, db, _ = await build(world, ("GET", "reports/summary", fn, None))
    r = await site.test_client(db).get(f"{BASE}/reports/summary")
    assert r.status == 401
    assert calls == []


async def test_a_route_needs_the_gate_and_its_permission_and_a_refusal_is_audited(world):
    site, db, models = await build(world, ("GET", "reports/summary", summary, None), ("GET", "reports/{id}", report, "admin.run_report"))
    ann = await grant(db, models, "ann", ["admin.access", "admin.run_report"])
    bob = await grant(db, models, "bob", ["admin.access"])
    cy = await user_id(db, models, "cy")
    assert (await site.test_client(db, user={"key": cy}).get(f"{BASE}/reports/summary")).status == 403
    assert (await site.test_client(db, user={"key": bob}).get(f"{BASE}/reports/summary")).json() == {"rows": 3}
    denied = await site.test_client(db, user={"key": bob}).get(f"{BASE}/reports/7")
    assert denied.status == 403
    assert [(a, res, ch["needs"]) for a, res, ch in await audit(db, models) if a == "permission_denied" and res.startswith("route:")] == [("permission_denied", "route:reports/{id}", ["admin.run_report"])]
    ok = await site.test_client(db, user={"key": ann}).get(f"{BASE}/reports/7")
    assert ok.status == 200
    assert ok.json() == {"id": "7", "user": ann, "has_db": True, "request_id": True}


async def test_the_route_permission_shows_in_permissions_and_an_unknown_label_is_a_build_error(world):
    registry, db, models = world
    site = admin.Admin(registry)
    site.register(models["Role"])
    site.route("GET", "a", summary, permission="admin.run_report")
    assert "admin.run_report" in site.permissions
    bad = admin.Admin(registry)
    bad.register(models["Role"])
    bad.route("GET", "a", summary, permission="nope.run_report")
    with pytest.raises(admin.AdminConfigError, match='unknown label "nope"'):
        bad.permissions


async def test_check_reports_an_unknown_route_label_as_a_build_error(world):
    registry, _, models = world
    bad = admin.Admin(registry)
    bad.register(models["Role"])
    bad.route("GET", "a", summary, permission="nope.run_report")
    with pytest.raises(admin.AdminConfigError, match='unknown label "nope"'):
        bad.check()


async def test_the_function_gets_params_query_and_the_json_body_and_a_reply_sets_the_status(world):
    seen = []

    async def rerun(ctx, request):
        seen.append((request.method, request.path, dict(request.params), dict(request.query), request.body))
        return admin.Reply({"queued": True}, status=202)

    site, db, _ = await build(world, ("POST", "reports/{id}/rerun", rerun, None))
    r = await site.test_client(db, user={"is_superuser": True, "key": 1}).post(f"{BASE}/reports/42/rerun?mode=fast&tag=a&tag=b", json={"limit": 5})
    assert (r.status, r.json()) == (202, {"queued": True})
    assert seen == [("POST", "reports/{id}/rerun", {"id": "42"}, {"mode": "fast", "tag": ["a", "b"]}, {"limit": 5})]


async def test_a_literal_path_wins_over_a_parameter_and_a_wrong_method_is_405_with_allow(world):
    async def literal(ctx, request):
        return "literal"

    async def param(ctx, request):
        return "param"

    site, db, _ = await build(world, ("GET", "reports/summary", literal, None), ("GET", "reports/{id}", param, None))
    client = site.test_client(db, user={"is_superuser": True, "key": 1})
    assert (await client.get(f"{BASE}/reports/summary")).json() == "literal"
    assert (await client.get(f"{BASE}/reports/other")).json() == "param"
    assert (await client.get(f"{BASE}/nothing/here")).status == 404
    wrong = await client.delete(f"{BASE}/reports/summary")
    assert wrong.status == 405
    assert dict(wrong.headers).get("allow") == "GET"


async def test_a_sync_function_and_a_function_that_fails(world):
    def blocking(ctx, request):
        return {"sync": True}

    async def broken(ctx, request):
        raise RuntimeError("secret detail")

    site, db, _ = await build(world, ("GET", "sync", blocking, None), ("GET", "broken", broken, None))
    client = site.test_client(db, user={"is_superuser": True, "key": 1})
    assert (await client.get(f"{BASE}/sync")).json() == {"sync": True}
    failed = await client.get(f"{BASE}/broken")
    assert failed.status == 500
    assert "secret detail" not in failed.text


async def test_a_stream_reaches_the_client_in_chunks_with_its_headers(world):
    async def rows(ctx, request):
        async def chunks():
            yield "id,name\n"
            for i in range(3):
                yield f"{i},row {i}\n".encode()

        return admin.Stream(chunks(), content_type="text/csv; charset=utf-8", filename="rows.csv")

    def lines(ctx, request):
        return admin.Stream((f"line {i}\n" for i in range(2000)), content_type="text/plain")

    site, db, _ = await build(world, ("GET", "rows", rows, None), ("GET", "lines", lines, None))
    client = site.test_client(db, user={"is_superuser": True, "key": 1})
    r = await client.get(f"{BASE}/rows")
    assert r.status == 200
    assert r.content == b"id,name\n0,row 0\n1,row 1\n2,row 2\n"
    headers = {k.lower(): v for k, v in r.headers}
    assert headers["content-type"] == "text/csv; charset=utf-8"
    assert headers["content-disposition"] == "attachment; filename=\"rows.csv\"; filename*=UTF-8''rows.csv"
    assert headers["cache-control"] == "no-store"
    big = await client.get(f"{BASE}/lines")
    assert big.content == "".join(f"line {i}\n" for i in range(2000)).encode()


async def test_a_login_client_needs_the_session_and_the_csrf_token_for_a_write(world):
    async def echo(ctx, request):
        return {"body": request.body}

    site, db, _ = await build(world, ("GET", "echo", echo, None), ("POST", "echo", echo, None))
    client = site.test_client(db, secret=SECRET)
    assert (await client.get(f"{BASE}/echo")).status == 401
    assert (await client.login("root", "pw")).status == 200
    assert (await client.get(f"{BASE}/echo")).json() == {"body": None}
    assert (await client.post(f"{BASE}/echo", json={"a": 1}, csrf="wrong")).status == 403
    assert (await client.post(f"{BASE}/echo", json={"a": 1})).json() == {"body": {"a": 1}}
