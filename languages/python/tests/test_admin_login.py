"""Password login, sessions and HTTP security of the admin, through `admin.test_client(secret=...)`."""

import os

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SECRET = "s" * 40
OTHER_SECRET = "o" * 40
ME = "/admin/api/me"
PASSWORD_URL = "/admin/api/password"
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
    await models["User"].objects.using(db).insert_many(
        [
            {"username": "ann", "email": "ann@example.com", "password": make_password("pw"), "is_superuser": True},
            {"username": "off", "email": "off@example.com", "password": make_password("pw"), "is_active": False},
        ]
    )
    yield admin.Admin(registry), db, models
    await db.drop_tables()
    await db.close()


async def audit(db, models):
    entry = models["AdminLogEntry"]
    rows = await entry.objects.using(db).order_by(entry.id)
    return [(r.user_key, r.action, r.record_label) for r in rows]


def header(reply, name):
    return next((v for k, v in reply.headers if k.lower() == name), None)


def cookie(reply, name):
    return next((v for k, v in reply.headers if k.lower() == "set-cookie" and v.startswith(name + "=")), None)


async def test_a_login_sets_a_signed_http_only_cookie_and_writes_an_audit_row(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET)
    reply = await client.login("ann", "pw")
    assert reply.status == 200, reply.text
    session = cookie(reply, "__Secure-monorm-session")
    assert session
    for flag in ("HttpOnly", "Secure", "SameSite=Lax", "Path=/admin/"):
        assert flag in session
    assert "Domain" not in session
    assert reply.json()["user"]["username"] == "ann"
    assert "password" not in reply.json()["user"]
    me = await client.get(ME)
    assert (me.status, me.json()["user"]["username"]) == (200, "ann")
    assert await audit(db, models) == [("1", "login", "ann")]


async def test_a_wrong_password_and_an_unknown_user_give_the_same_401_and_a_failed_login_row(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET)
    wrong = await client.login("ann", "nope")
    unknown = await client.login("zed", "nope")
    assert (wrong.status, unknown.status) == (401, 401)
    assert wrong.json() == unknown.json()
    # the typed text is no account: the user key stays empty and the label keeps the name
    assert await audit(db, models) == [("", "login_failed", "ann"), ("", "login_failed", "zed")]


async def test_an_inactive_user_cannot_log_in(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    assert (await client.login("off", "pw")).status == 401
    assert (await client.get(ME)).status == 401


async def test_a_request_without_a_session_gets_401_before_404(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    for path in ("/admin/api/resources", "/admin/api/nothing", "/elsewhere", ME):
        assert (await client.get(path)).status == 401, path
    await client.login("ann", "pw")
    assert (await client.get("/admin/api/nothing")).status == 404
    assert (await client.get("/admin/api/resources")).status == 200


async def test_five_failures_for_a_user_from_one_ip_answer_429_but_another_ip_still_signs_in(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET, ip="10.0.1.1")
    for _ in range(5):
        assert (await client.login("ann", "nope")).status == 401
    blocked = await client.login("ann", "pw")
    assert blocked.status == 429
    assert header(blocked, "retry-after")
    assert (await client.login("other", "nope")).status == 401
    client.ip = "10.0.1.9"
    assert (await client.login("ann", "pw")).status == 200, "an attacker cannot lock the owner out from one address"


async def test_fifty_failures_for_one_name_over_many_ips_answer_429_for_every_ip(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    for i in range(10):
        client.ip = f"10.0.4.{i}"
        for _ in range(5):
            assert (await client.login("ann", "nope")).status == 401
    client.ip = "10.0.4.99"
    assert (await client.login("ann", "pw")).status == 429
    assert (await client.login("other", "nope")).status == 401


async def test_a_blocked_login_writes_a_login_blocked_row(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET, ip="10.0.6.1")
    for _ in range(5):
        await client.login("ann", "nope")
    for _ in range(3):
        assert (await client.login("ann", "pw")).status == 429
    blocked = [row for row in await audit(db, models) if row[1] == "login_blocked"]
    assert blocked == [("", "login_blocked", "ann")], "three attempts, one row"


async def login_through(client, forwarded, password):
    await client.get("/admin/api/login")
    return await client.post("/admin/api/login", json={"username": "ann", "password": password}, headers={"X-Forwarded-For": forwarded})


async def test_x_forwarded_for_counts_only_from_a_trusted_proxy(world):
    site, db, _ = world
    plain = site.test_client(db, secret=SECRET, ip="10.0.5.1")
    for i in range(20):
        await plain.login(f"user{i}", "nope")
    assert (await login_through(plain, "198.51.100.200", "pw")).status == 429, "an untrusted peer cannot choose its address"
    proxied = site.test_client(db, secret=SECRET, ip="10.0.5.9", settings={"trusted_proxies": ["10.0.5.9"]})
    for _ in range(5):
        assert (await login_through(proxied, "203.0.113.5", "nope")).status == 401
    assert (await login_through(proxied, "203.0.113.5", "pw")).status == 429
    assert (await login_through(proxied, "203.0.113.6", "pw")).status == 200


async def test_a_trusted_proxy_that_is_no_address_is_a_value_error(world):
    site, db, _ = world
    with pytest.raises(ValueError, match="trusted_proxies"):
        await site.test_client(db, secret=SECRET, settings={"trusted_proxies": ["not-an-ip"]}).get(ME)


async def test_twenty_failures_from_one_ip_answer_429_for_any_user(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET, ip="10.0.2.2")
    for i in range(20):
        assert (await client.login(f"user{i}", "nope")).status == 401
    assert (await client.login("ann", "pw")).status == 429
    client.ip = "10.0.2.3"
    assert (await client.login("ann", "pw")).status == 200


async def test_a_write_needs_the_csrf_token_the_origin_and_json(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    await client.login("ann", "pw")
    body = {"old_password": "pw", "new_password": "new-password"}
    no_token = await client.post(PASSWORD_URL, json=body, csrf=None)
    assert (no_token.status, no_token.json()["error"]["kind"]) == (403, "BadCsrfToken")
    assert (await client.post(PASSWORD_URL, json=body, csrf="abc.def")).status == 403
    no_origin = await client.post(PASSWORD_URL, json=body, origin=None)
    assert (no_origin.status, no_origin.json()["error"]["kind"]) == (403, "BadOrigin")
    assert (await client.post(PASSWORD_URL, json=body, origin="https://evil.example")).status == 403
    referer = await client.post(PASSWORD_URL, json=body, origin=None, headers={"Referer": "http://localhost/admin/x"})
    assert referer.status == 200
    form = await client.post(PASSWORD_URL, headers={"Content-Type": "application/x-www-form-urlencoded"})
    assert (form.status, form.json()["error"]["kind"]) == (415, "UnsupportedMediaType")
    other = site.test_client(db, secret=SECRET, ip="10.0.3.3")
    await other.login("ann", "pw")
    swapped = await client.post(PASSWORD_URL, json=body, csrf=other.cookies["__Secure-monorm-csrf"])
    assert swapped.status == 403, "a token is bound to its session"


async def test_a_configured_origin_is_accepted_and_the_login_needs_csrf_too(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET, settings={"origins": ["https://admin.example.com"]})
    await client.get("/admin/api/login")
    credentials = {"username": "ann", "password": "pw"}
    assert (await client.post("/admin/api/login", json=credentials, csrf=None)).status == 403
    assert (await client.post("/admin/api/login", json=credentials, origin="https://admin.example.com")).status == 200


async def test_every_response_has_the_security_headers(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    replies = [await client.get("/admin/api/resources"), await client.get("/healthz"), await client.get("/admin/api/login")]
    await client.login("ann", "pw")
    replies += [await client.get("/admin/api/nothing"), await client.get("/admin/api/resources"), await client.post(PASSWORD_URL)]
    for reply in replies:
        assert "default-src 'none'" in header(reply, "content-security-policy")
        assert header(reply, "x-frame-options") == "DENY"
        assert header(reply, "referrer-policy") == "same-origin"
        assert header(reply, "cache-control") == "no-store"


async def test_two_clients_with_the_same_secret_share_a_session_and_another_secret_does_not(world):
    site, db, _ = world
    first = site.test_client(db, secret=SECRET)
    await first.login("ann", "pw")
    second = site.test_client(db, secret=SECRET, ip="10.0.4.4")
    second.cookies.update(first.cookies)
    assert (await second.get(ME)).status == 200
    foreign = site.test_client(db, secret=OTHER_SECRET, ip="10.0.4.5")
    foreign.cookies.update(first.cookies)
    assert (await foreign.get(ME)).status == 401
    tampered = site.test_client(db, secret=SECRET, ip="10.0.4.6")
    tampered.cookies.update(first.cookies)
    tampered.cookies["__Secure-monorm-session"] += "x"
    assert (await tampered.get(ME)).status == 401


async def test_the_idle_timeout_ends_a_session(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET, settings={"idle_timeout": 1})
    await client.login("ann", "pw")
    assert (await client.get(ME)).status == 200
    client.advance_clock(3)
    assert (await client.get(ME)).status == 401


async def test_the_absolute_timeout_ends_a_session_that_is_in_use(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET, settings={"session_timeout": 2})
    await client.login("ann", "pw")
    for _ in range(2):
        assert (await client.get(ME)).status == 200
        client.advance_clock(1)
    client.advance_clock(1)
    assert (await client.get(ME)).status == 401


async def test_logout_ends_the_session_even_for_a_copy_of_the_cookie(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    await client.login("ann", "pw")
    copy = dict(client.cookies)
    assert (await client.logout()).status == 200
    assert (await client.get(ME)).status == 401
    client.cookies.update(copy)
    assert (await client.get(ME)).status == 401, "the revoked session stays revoked in this process"


RENEWS = {"idle_timeout": 4}  # a session is checked again every second (a quarter of the idle time)


async def change_ann(db, models, **values):
    user_model = models["User"]
    ann = await user_model.objects.using(db).filter(user_model.username == "ann").first()
    await ann.update(**values)


async def test_a_deactivated_user_loses_the_session_at_the_next_renewal(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET, settings=RENEWS)
    await client.login("ann", "pw")
    await change_ann(db, models, is_active=False)
    assert (await client.get(ME)).status == 200, "the change shows at the renewal, not before"
    client.advance_clock(2)
    assert (await client.get(ME)).status == 401
    assert (await client.get(ME)).status == 401


async def test_a_demoted_superuser_loses_the_session_at_the_next_renewal(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET, settings=RENEWS)
    await client.login("ann", "pw")
    await change_ann(db, models, is_superuser=False)
    client.advance_clock(2)
    assert (await client.get(ME)).status == 401


async def test_a_password_changed_outside_the_service_ends_the_session_at_the_next_renewal(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET, settings=RENEWS)
    await client.login("ann", "pw")
    await change_ann(db, models, password=make_password("changed-elsewhere"))
    client.advance_clock(2)
    assert (await client.get(ME)).status == 401


async def test_a_logout_in_one_client_ends_the_copy_of_the_cookie_in_another_client(world):
    site, db, _ = world
    first = site.test_client(db, secret=SECRET, settings=RENEWS)
    await first.login("ann", "pw")
    second = site.test_client(db, secret=SECRET, ip="10.0.7.7", settings=RENEWS)
    second.cookies.update(first.cookies)
    assert (await second.get(ME)).status == 200
    assert (await first.logout()).status == 200
    assert (await second.get(ME)).status == 200, "the second client learns of the logout at its next renewal"
    second.advance_clock(2)
    assert (await second.get(ME)).status == 401


async def test_a_password_change_ends_the_other_sessions_and_keeps_the_changed_one(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET, settings=RENEWS)
    await client.login("ann", "pw")
    old = dict(client.cookies)
    elsewhere = site.test_client(db, secret=SECRET, ip="10.0.8.8", settings=RENEWS)
    await elsewhere.login("ann", "pw")
    done = await client.post(PASSWORD_URL, json={"old_password": "pw", "new_password": "new-password"})
    assert done.status == 200
    assert cookie(done, "__Secure-monorm-session") and done.json()["csrf"]
    assert client.cookies["__Secure-monorm-session"] != old["__Secure-monorm-session"]
    client.cookies.update(old)
    assert (await client.get(ME)).status == 401, "the old cookie is dead at once"
    await client.login("ann", "new-password")
    client.advance_clock(2)
    elsewhere.advance_clock(2)
    assert (await client.get(ME)).status == 200, "the new session knows the new hash"
    assert (await elsewhere.get(ME)).status == 401


async def test_a_password_change_needs_the_old_password_and_writes_an_audit_row(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET)
    await client.login("ann", "pw")
    wrong = await client.post(PASSWORD_URL, json={"old_password": "nope", "new_password": "new-password"})
    assert (wrong.status, wrong.json()["error"]["kind"]) == (400, "InvalidPassword")
    assert (await client.post(PASSWORD_URL, json={"new_password": "new-password"})).status == 400
    assert (await client.post(PASSWORD_URL, json={"old_password": "pw", "new_password": "new-password"})).status == 200
    assert [row[1] for row in await audit(db, models)] == ["login", "password_change_failed", "password_change"]
    assert (await site.test_client(db, secret=SECRET, ip="10.0.6.6").login("ann", "pw")).status == 401
    assert (await site.test_client(db, secret=SECRET, ip="10.0.6.7").login("ann", "new-password")).status == 200


async def test_a_weak_new_password_is_refused_with_the_rule_codes_and_the_audit_row_has_no_password(world):
    site, db, models = world
    client = site.test_client(db, secret=SECRET)
    await client.login("ann", "pw")
    weak = await client.post(PASSWORD_URL, json={"old_password": "pw", "new_password": "1234"})
    error = weak.json()["error"]
    assert (weak.status, error["kind"]) == (400, "WeakPassword")
    assert [e["code"] for e in error["errors"]] == ["password_too_short", "password_too_common", "password_entirely_numeric"]
    assert error["errors"][0]["params"] == {"min": 8}
    assert error["message"].startswith("This password is too short.")
    entry = models["AdminLogEntry"]
    last = (await entry.objects.using(db).order_by(entry.id.desc()))[0]
    assert last.action == "password_change_failed"
    assert "password_too_short" in str(last.changes) and "1234" not in str(last.changes)
    assert (await site.test_client(db, secret=SECRET, ip="10.0.6.8").login("ann", "pw")).status == 200


async def test_weak_new_passwords_do_not_use_up_the_password_limit_but_wrong_old_passwords_do(world):
    site, db, _ = world
    client = site.test_client(db, secret=SECRET)
    await client.login("ann", "pw")
    for _ in range(7):
        assert (await client.post(PASSWORD_URL, json={"old_password": "pw", "new_password": "1234"})).status == 400
    for _ in range(5):
        wrong = await client.post(PASSWORD_URL, json={"old_password": "no", "new_password": "a-long-new-password"})
        assert wrong.json()["error"]["kind"] == "InvalidPassword"
    assert (await client.post(PASSWORD_URL, json={"old_password": "pw", "new_password": "a-long-new-password"})).status == 429


async def test_the_user_client_needs_no_login_and_does_not_mix_with_a_secret(world):
    site, db, _ = world
    assert (await site.test_client(db, user={"is_superuser": True}).get("/admin/api/resources")).status == 200
    assert (await site.test_client(db).get("/admin/api/resources")).status == 401
    with pytest.raises(ValueError):
        site.test_client(db, user={"is_superuser": True}, secret=SECRET)


async def test_a_missing_or_short_secret_is_a_value_error(world):
    site, db, _ = world
    with pytest.raises(ValueError, match="MONORM_ADMIN_SECRET"):
        await site.test_client(db, secret="short").get(ME)


def test_set_password_needs_the_auth_user_model(world):
    site, _, models = world
    with pytest.raises(admin.AdminConfigError, match="set_password.*needs the auth user model"):
        site.register(models["AdminLogEntry"], {"set_password": True})
        site.check()


def test_set_password_action_cannot_replace_the_resource_change_rule(world):
    site, _, models = world
    user = models["User"]

    @admin.action(placement="detail", can=user.id > 0, replace_change=True)
    async def set_password(ctx, qs):
        raise AssertionError("Rust runs the built-in action")

    site.register(user, {"set_password": True, "actions": [set_password]})
    with pytest.raises(admin.AdminConfigError, match="set_password always requires the resource change rule"):
        site.check()


async def test_set_password_keeps_the_current_session_and_ends_the_other_sessions(world, capfd):
    site, db, models = world
    site.register(models["User"], {"set_password": True})
    actor = site.test_client(db, secret=SECRET)
    other = site.test_client(db, secret=SECRET)
    assert (await actor.login("ann", "pw")).status == 200
    assert (await other.login("ann", "pw")).status == 200
    path = "/admin/api/actions/User/set_password"
    entry = models["AdminLogEntry"]
    users = models["User"].objects.using(db)
    old_hash = (await users.filter(models["User"].id == 1).first()).password

    async def reset(first, second, keys=("1",), *, statements):
        queries = []
        stop = db.on_query(queries.append)
        try:
            reply = await actor.post(path, json={"keys": list(keys), "inputs": {"new_password": first, "new_password_again": second}})
        finally:
            stop()
        assert len(queries) == statements
        return reply

    assert (await reset("new-password", "different", statements=1)).json()["error"]["kind"] == "PasswordMismatch"
    assert (await reset("new-password", "new-password", ("1", "2"), statements=1)).status == 400
    assert [row[1] for row in await audit(db, models)] == ["login", "login"]
    weak = await reset("password", "password", statements=3)
    assert (weak.status, weak.json()["error"]["kind"]) == (400, "WeakPassword")
    assert [e["code"] for e in weak.json()["error"]["errors"]] == ["password_too_common"]
    assert (await users.filter(models["User"].id == 1).first()).password == old_hash
    new = "new-private-password"
    done = await reset(new, new, statements=4)
    assert done.status == 200, done.text
    assert new not in done.text
    assert cookie(done, "__Secure-monorm-session")
    assert (await actor.get(ME)).status == 200
    assert (await other.get(ME)).status == 401
    assert (await users.filter(models["User"].id == 1).first()).check_password(new)
    rows = await entry.objects.using(db).filter(entry.action.in_(["password_set", "password_set_failed"])).order_by(entry.id)
    assert [(r.action, r.user_key, r.resource, r.record_key) for r in rows] == [
        ("password_set_failed", "1", "User", "1"), ("password_set", "1", "User", "1")
    ]
    assert rows[0].changes["codes"] == [e["code"] for e in weak.json()["error"]["errors"]]
    assert all(set(r.changes) <= {"ip", "request_id", "codes"} for r in rows)
    assert new not in str([r.changes for r in rows])
    assert all(new not in stream for stream in capfd.readouterr())


async def test_set_password_requires_change_and_row_scope_and_guards_superusers(world):
    site, db, models = world
    user = models["User"]
    entry = models["AdminLogEntry"]
    site.register(user, {
        "set_password": True,
        "queryset": user.objects.filter(user.id < 5).prepare(),
        "can": {"change": user.id != 3},
    })
    await site.sync_permissions(db)
    original = make_password("pw")
    users = user.objects.using(db)
    await users.insert_many(
        [{"username": name, "email": f"{name}@example.com", "password": original} for name in ("staff", "target", "outside")]
    )
    perms = models["Permission"]
    granted = await perms.objects.using(db).filter(perms.codename.in_(["access", "read_user"]))
    role = (await models["Role"].objects.using(db).insert_many([{"name": "staff"}]).returning())[0]
    role_permissions = models["RolePermission"].objects.using(db)
    await role_permissions.insert_many([{"role_id": role.id, "permission_id": p.id} for p in granted])
    await models["UserRole"].objects.using(db).insert_many([{"user_id": 3, "role_id": role.id}])

    staff = site.test_client(db, secret=SECRET)
    target = site.test_client(db, secret=SECRET)
    assert (await staff.login("staff", "pw")).status == 200
    assert (await target.login("target", "pw")).status == 200
    password = "staff-issued-password"

    async def reset(key, status, statements):
        before = (await users.filter(user.id == key).first()).password
        queries = []
        stop = db.on_query(queries.append)
        try:
            reply = await staff.post("/admin/api/actions/User/set_password", json={
                "keys": [str(key)], "inputs": {"new_password": password, "new_password_again": password},
            })
        finally:
            stop()
        assert reply.status == status, reply.text
        assert len(queries) == statements
        stored = await users.filter(user.id == key).first()
        if status == 200:
            assert stored.check_password(password)
        else:
            assert stored.password == before

    await reset(4, 403, 1)
    change = await perms.objects.using(db).filter(perms.codename == "manage_user").first()
    await role_permissions.insert_many([{"role_id": role.id, "permission_id": change.id}])
    await reset(5, 404, 1)
    await reset(3, 403, 1)
    await reset(1, 403, 2)
    await reset(4, 200, 4)
    assert (await target.get(ME)).status == 401
    await reset(2, 200, 4)
    rows = await entry.objects.using(db).order_by(entry.id)
    assert [(r.action, r.user_key, r.record_key) for r in rows if r.action != "login"] == [
        ("permission_denied", "3", ""),
        ("action_denied", "3", "3"),
        ("password_set_failed", "3", "1"),
        ("password_set", "3", "4"),
        ("password_set", "3", "2"),
    ]


async def test_set_password_applies_the_action_rule_and_counts_the_transaction_queries(world):
    site, db, models = world
    user = models["User"]
    entry = models["AdminLogEntry"]

    @admin.action(placement="detail", can=user.id == 1)
    async def set_password(ctx, qs):
        raise AssertionError("Rust runs the built-in action")

    site.register(user, {"set_password": True, "actions": [set_password]})
    client = site.test_client(db, user={"key": 1, "is_superuser": True})
    password = "bounded-query-password"
    path = "/admin/api/actions/User/set_password"

    async def reset(key):
        return await client.post(path, json={
            "keys": [key], "inputs": {"new_password": password, "new_password_again": password},
        })

    preamble = (await client.get("/admin/api/resources")).queries
    refused = await reset("2")
    assert (refused.status, refused.queries) == (403, preamble + 2), refused.sql
    done = await reset("1")
    assert (done.status, done.queries) == (200, preamble + 6), done.sql
    users = user.objects.using(db)
    assert (await users.filter(user.id == 1).first()).check_password(password)
    assert (await users.filter(user.id == 2).first()).check_password("pw")
    rows = await entry.objects.using(db)
    assert [(r.action, r.record_key) for r in rows] == [("action_denied", "2"), ("password_set", "1")]
