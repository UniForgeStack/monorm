"""OIDC single sign-on of the admin: the authorization-code flow against a fake provider, roles from the claim, the audit rows."""

import os
from urllib.parse import urlsplit

import pytest
from fake_idp import CLIENT_ID, FakeIdp

import monorm
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SECRET = "s" * 40
ME = "/admin/api/me"
START = "/admin/auth/start"
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
            {"username": "ann@example.com", "email": "ann@example.com", "password": make_password("pw")},
            {"username": "off@example.com", "email": "off@example.com", "password": make_password("pw"), "is_active": False},
        ]
    )
    await models["Role"].objects.using(db).insert_many([{"name": "editor"}, {"name": "auditor"}])
    yield admin.Admin(registry), db, models
    await db.drop_tables()
    await db.close()


@pytest.fixture
def idp():
    with FakeIdp() as provider:
        yield provider


async def audit(db, models):
    entry = models["AdminLogEntry"]
    rows = await entry.objects.using(db).order_by(entry.id)
    return [(r.action, r.record_label, r.changes) for r in rows]


async def roles_of(db, models, username):
    user = await models["User"].objects.using(db).filter(models["User"].username == username).first()
    assert user is not None
    link = models["UserRole"]
    role = models["Role"]
    ids = [j.role_id for j in await link.objects.using(db).filter(link.user_id == user.id)]
    return sorted(r.name for r in await role.objects.using(db).filter(role.id.in_(ids))) if ids else []


def location(reply):
    return next(v for k, v in reply.headers if k.lower() == "location")


async def sign_in(client, idp, **claims):
    """The browser round trip: the start, the provider, the callback. Returns the reply of the callback."""
    start = await client.get(START)
    assert start.status == 302, start.text
    return await client.get(idp.approve(location(start), **claims))


def site_of(world, idp, **extra):
    site, db, _ = world
    return site.test_client(db, secret=SECRET, settings=idp.settings(**extra))


async def test_the_start_redirects_to_the_provider_with_state_nonce_and_pkce(world, idp):
    client = site_of(world, idp, scopes=["groups"])
    reply = await client.get(START)
    assert reply.status == 302
    target = urlsplit(location(reply))
    assert f"{target.scheme}://{target.netloc}" == idp.issuer and target.path == "/authorize"
    query = dict(p.split("=", 1) for p in target.query.split("&"))
    assert query["client_id"] == CLIENT_ID and query["response_type"] == "code"
    assert query["scope"] == "openid+profile+email+groups"
    assert query["redirect_uri"].endswith("%2Fadmin%2Fauth%2Fcallback")
    assert query["code_challenge_method"] == "S256" and len(query["state"]) >= 22 and len(query["nonce"]) >= 22
    assert any(k.startswith("__Secure-monorm-oidc=") for k, v in reply.headers if k.lower() == "set-cookie" for k in [v])


async def test_the_login_page_names_the_sso_start_only_when_oidc_is_set(world, idp):
    site, db, _ = world
    plain = site.test_client(db, secret=SECRET)
    assert "sso" not in (await plain.get("/admin/api/login")).json()
    assert (await site_of(world, idp).get("/admin/api/login")).json()["sso"] == START


async def test_a_login_of_a_new_user_creates_it_and_sets_the_roles_of_the_claim(world, idp):
    _, db, models = world
    client = site_of(world, idp)
    done = await sign_in(client, idp, email="new@example.com", roles=["editor", "nobody"])
    assert (done.status, location(done)) == (302, "/admin/")
    me = await client.get(ME)
    assert (me.status, me.json()["user"]["username"]) == (200, "new@example.com")
    assert await roles_of(db, models, "new@example.com") == ["editor"]
    created = await models["User"].objects.using(db).filter(models["User"].username == "new@example.com").first()
    assert created is not None and created.email == "new@example.com" and not created.is_superuser
    assert not created.check_password("")
    [(action, label, changes)] = await audit(db, models)
    assert (action, label) == ("login", "new@example.com")
    assert changes["method"] == "sso" and changes["created"] is True


async def test_the_roles_follow_the_claim_at_each_login_and_a_user_without_a_match_has_none(world, idp):
    _, db, models = world
    client = site_of(world, idp)
    await sign_in(client, idp, email="ann@example.com", roles=["editor", "auditor"])
    assert await roles_of(db, models, "ann@example.com") == ["auditor", "editor"]
    await client.logout()
    await sign_in(client, idp, email="ann@example.com", roles=["auditor"])
    assert await roles_of(db, models, "ann@example.com") == ["auditor"]
    await client.logout()
    await sign_in(client, idp, email="ann@example.com", roles=["unknown"])
    assert await roles_of(db, models, "ann@example.com") == []
    await client.logout()
    await sign_in(client, idp, email="ann@example.com")
    assert await roles_of(db, models, "ann@example.com") == []
    assert [a for a, _, _ in await audit(db, models) if a == "login"] == ["login"] * 4 and (await client.get(ME)).status == 200


async def test_the_roles_claim_can_be_one_name_or_a_nested_path(world, idp):
    _, db, models = world
    client = site_of(world, idp, roles_claim="realm.roles")
    await sign_in(client, idp, email="ann@example.com", realm={"roles": ["editor"]})
    assert await roles_of(db, models, "ann@example.com") == ["editor"]
    await client.logout()
    client = site_of(world, idp)
    await sign_in(client, idp, email="ann@example.com", roles="auditor")
    assert await roles_of(db, models, "ann@example.com") == ["auditor"]


async def test_an_inactive_user_cannot_log_in_and_its_roles_stay(world, idp):
    _, db, models = world
    client = site_of(world, idp)
    done = await sign_in(client, idp, email="off@example.com", roles=["editor"])
    assert location(done) == "/admin/?sso_error=inactive"
    assert (await client.get(ME)).status == 401
    assert await roles_of(db, models, "off@example.com") == []
    [(action, label, changes)] = await audit(db, models)
    assert (action, label, changes["reason"]) == ("login_failed", "off@example.com", "inactive")


async def test_a_wrong_state_is_refused_and_writes_a_failed_login_row(world, idp):
    _, db, models = world
    client = site_of(world, idp)
    done = await sign_in(client, idp, state="not-the-state", email="ann@example.com")
    assert location(done) == "/admin/?sso_error=failed"
    assert (await client.get(ME)).status == 401
    [(action, _, changes)] = await audit(db, models)
    assert (action, changes["reason"]) == ("login_failed", "bad_state")


async def test_a_callback_without_a_started_flow_is_refused(world, idp):
    client = site_of(world, idp)
    done = await client.get("/admin/auth/callback?code=x&state=y")
    assert location(done) == "/admin/?sso_error=failed"


async def test_a_code_works_once(world, idp):
    client = site_of(world, idp)
    start = await client.get(START)
    callback = idp.approve(location(start), email="ann@example.com")
    assert location(await client.get(callback)) == "/admin/"
    await client.logout()
    again = await client.get(callback)
    assert location(again) == "/admin/?sso_error=failed"
    assert (await client.get(ME)).status == 401


@pytest.mark.parametrize(
    ("override", "word"),
    [
        ({"nonce": "other"}, "failed"),
        ({"aud": "someone-else"}, "failed"),
        ({"iss": "https://evil.example.com"}, "failed"),
        ({"exp": 1}, "failed"),
        ({"email": None}, "failed"),
    ],
)
async def test_a_token_with_a_wrong_claim_is_refused(world, idp, override, word):
    _, db, models = world
    idp.token_overrides = override
    client = site_of(world, idp)
    done = await sign_in(client, idp, email="ann@example.com", roles=["editor"])
    assert location(done) == f"/admin/?sso_error={word}"
    assert (await client.get(ME)).status == 401
    assert await roles_of(db, models, "ann@example.com") == []
    assert [a for a, _, _ in await audit(db, models)] == ["login_failed"]


async def test_the_hosted_domain_is_checked_on_the_server(world, idp):
    client = site_of(world, idp, hd="example.com")
    assert location(await sign_in(client, idp, email="ann@example.com", hd="other.com")) == "/admin/?sso_error=domain"
    assert location(await sign_in(client, idp, email="ann@example.com")) == "/admin/?sso_error=domain"
    assert (await client.get(ME)).status == 401
    assert location(await sign_in(client, idp, email="ann@example.com", hd="example.com")) == "/admin/"
    assert (await client.get(ME)).status == 200


async def test_a_provider_error_and_an_unreachable_provider_send_the_browser_back(world, idp):
    _, db, models = world
    client = site_of(world, idp)
    assert location(await client.get("/admin/auth/callback?error=access_denied")) == "/admin/?sso_error=denied"
    idp.fail_token = True
    assert location(await sign_in(client, idp, email="ann@example.com")) == "/admin/?sso_error=unavailable"
    assert [a for a, _, _ in await audit(db, models)] == ["login_failed", "login_failed"]
    assert (await client.get(ME)).status == 401


async def test_the_flow_ends_with_the_callback(world, idp):
    client = site_of(world, idp)
    await sign_in(client, idp, email="ann@example.com")
    assert "__Secure-monorm-oidc" not in client.cookies

