"""Frame and embed configuration and requests on SQLite and PostgreSQL."""

import os
from urllib.parse import urlencode, urlsplit

import pytest

import monorm
from monorm import admin

from test_admin_field_permissions import grant

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
BASE = "/back/admin"
API = f"{BASE}/api"
ITEMS = f"{API}/resources/FrameItem"
EMBED = f"{API}/embeds/health"
SPECIAL = "é /?#%&=+"
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")

model FrameOwner {
  id    Int @id
  name  String
  items FrameItem[]
}

model FrameItem {
  id         Int @id
  domain     String @unique
  title      String
  secret     String?
  payload    Json?
  owner_id   Int?
  owner      FrameOwner? @relation(fields: [owner_id], references: [id])
}
"""


@pytest.fixture(params=["sqlite", "postgres"])
async def world(request, tmp_path):
    schema = tmp_path / "schema.prisma"
    schema.write_text(SCHEMA % request.param)
    registry = monorm.Registry()
    models = monorm.load(schema, registry=registry)
    url = "sqlite://:memory:" if request.param == "sqlite" else PG_URL
    try:
        db = await monorm.connect(url, registry=registry, default=False)
    except monorm.DatabaseError:
        if "ORM_TEST_DATABASE_URL" in os.environ:
            raise
        pytest.skip("PostgreSQL is unavailable")
    await db.drop_tables()
    await db.create_tables()
    try:
        await models["User"].objects.using(db).insert_many([
            {"username": "reader", "email": "reader@example.com", "password": "unused"},
            {"username": "denied", "email": "denied@example.com", "password": "unused"},
        ])
        await models["FrameOwner"].objects.using(db).insert_many([{"id": 1, "name": "Private owner"}])
        await models["FrameItem"].objects.using(db).insert_many([
            {"id": 1, "domain": SPECIAL, "title": "Healthy", "secret": "Internal", "owner_id": 1},
            {"id": 2, "domain": "nullable", "title": "Empty"},
            {"id": 3, "domain": "outside", "title": "Out of scope"},
        ])
        yield registry, models, db
    finally:
        await db.drop_tables()
        await db.close()


def frame(url, **extra):
    return {"name": "health", "title": "Health frame", "url": url, **extra}


def embed(**extra):
    return {"resource": "shops.FrameItem", "lookup": "domain", "tab": "health_info", "ancestors": ["'self'"], **extra}


def build(world, options=None, resource=None):
    registry, models, _ = world
    site = admin.Admin(registry, base_path=BASE, **(options or {}))
    site.register(models["FrameItem"], {
        "see": {"secret": "see_secret"},
        "fieldsets": [{"name": "health_info", "title": "Health", "fields": ["title", "secret"]}],
        **(resource or {}),
    })
    site.register(models["FrameOwner"], {"see": {"name": "see_owner_name"}})
    return site


async def test_frame_config_errors(world, tmp_path):
    for origin in ["https://example.com/path", "https://example.com/", "https://*.example.com", "http://example.com", "https://example.com;"]:
        with pytest.raises(admin.AdminConfigError, match="invalid frame origin"):
            build(world, {"frame_origins": [origin]}).check()

    for url, problem in [
        ("//evil.example/path", "invalid frame URL"),
        ("https://other.example/path", "frame_origins"),
        ("https://{domain}.example/path", "invalid frame URL"),
        ("/identity?{domain}=x", "invalid frame URL"),
        ("/identity?x=prefix{domain}", "invalid frame URL"),
        ("/identity#{domain}", "invalid frame URL"),
        ("/identity/{payload}", "must be scalar"),
        ("/identity/{misspelled}", "unknown field"),
        ("https://example.com:{domain}/identity", "invalid frame URL"),
        ("/outside/../back/admin/FrameItem", "points into this admin"),
    ]:
        with pytest.raises(admin.AdminConfigError, match=problem):
            build(world, {"frame_origins": ["'self'"]}, {"frames": [frame(url)]}).check()

    with pytest.raises(admin.AdminConfigError, match="frame_origins"):
        build(world, resource={"frames": [frame("/identity")]}).check()
    with pytest.raises(admin.AdminConfigError, match="sandbox flag"):
        build(world, {"frame_origins": ["'self'"]}, {"frames": [frame("/identity", sandbox=["allow-top-navigation"])]}).check()
    with pytest.raises(admin.AdminConfigError, match="cannot have placeholders"):
        build(world, {"frame_origins": ["'self'"], "frame_cards": [frame("/identity/{domain}")]}).check()
    origins = {"frame_origins": ["'self'"]}
    for resource, problem in [
        ({"frames": [frame("/identity", name="Invalid")]}, "frame name"),
        ({"frames": [frame("/identity", title="")]}, "unique name and a title"),
        ({"frames": [frame("/identity", name="health_info")]}, "duplicate detail tab"),
        ({"fieldsets": [{"name": "Invalid", "fields": ["title"]}]}, "fieldset name"),
        ({"fieldsets": [{"name": "empty", "fields": []}]}, "at least one field"),
        ({"fieldsets": [{"name": "missing", "fields": ["missing"]}]}, "unknown or repeated field"),
    ]:
        with pytest.raises(admin.AdminConfigError, match=problem):
            build(world, origins, resource).check()

    with pytest.raises(admin.AdminConfigError, match="embed name"):
        build(world, {"embeds": {"Invalid": embed()}}).check()
    for config, problem in [
        (embed(lookup="title"), "unique scalar field"),
        (embed(tab="missing"), "unknown detail tab"),
        (embed(resource="Missing"), "unknown resource"),
        (embed(ancestors=[]), "at least one origin"),
        (embed(ancestors=["https://parent.example/path"]), "invalid frame origin"),
    ]:
        with pytest.raises(admin.AdminConfigError, match=problem):
            build(world, {"embeds": {"health": config}}).check()

    (tmp_path / "summary.js").write_text("export const adminMajor = 1;")
    with pytest.raises(admin.AdminConfigError, match="embeddable: true"):
        site = build(world, {"embeds": {"health": embed(tab="reports.summary")}}, {"detail_tabs": ["reports.summary"]})
        site.package("reports", tmp_path, tabs={"summary": "summary.js"})
        site.check()


async def test_frame_headers_and_home_cards(world):
    _, _, db = world
    opts = {
        "frame_origins": ["'self'", "https://charts.example:8443", "http://localhost:1234"],
        "frame_cards": [frame("https://charts.example:8443/health", scripts=False, same_origin=False)],
        "embeds": {"health": embed(), "external": embed(ancestors=["'self'", "https://portal.example"])},
    }
    site = build(world, opts)
    browser = site.test_client(db, secret="a" * 40)
    for path, status, ancestors, xfo in [
        (BASE, 200, "'none'", "DENY"),
        (f"{BASE}/FrameItem/1", 200, "'none'", "DENY"),
        (f"{BASE}/-/embed/health", 200, "'self'", "SAMEORIGIN"),
        (f"{BASE}/-/embed/external", 200, "'self' https://portal.example", None),
        (f"{BASE}/-/embed/missing", 404, "'none'", "DENY"),
        (f"{BASE}/-/embed/health/extra", 404, "'none'", "DENY"),
        (f"{API}/embeds/health", 401, "'none'", "DENY"),
    ]:
        response = await browser.get(path)
        headers = dict(response.headers)
        assert response.status == status, path
        assert f"frame-ancestors {ancestors}" in headers["content-security-policy"].split("; ")
        assert headers.get("x-frame-options") == xfo
        if status == 200:
            assert "frame-src 'self' https://charts.example:8443 http://localhost:1234" in headers["content-security-policy"]
        else:
            assert "frame-src" not in headers["content-security-policy"]
    no_frames = await build(world).test_client(db, secret="b" * 40).get(BASE)
    assert "frame-src" not in dict(no_frames.headers)["content-security-policy"]
    home = await site.test_client(db, user={"is_superuser": True}).get(f"{API}/site")
    assert home.json()["frame_cards"] == [{
        "name": "health", "title": "Health frame", "url": "https://charts.example:8443/health",
        "sandbox": "", "height": "70vh", "referrer_policy": "no-referrer",
    }]


async def test_frame_detail_and_embed_obey_permissions_and_scope(world, tmp_path):
    _, models, db = world
    item = models["FrameItem"]
    opts = {"frame_origins": ["'self'"], "embeds": {"health": embed(), "package": embed(tab="reports.summary")}}
    site = build(world, opts, {
        "queryset": item.objects.filter(item.id < 3).prepare(),
        "frames": [
            frame("/identity/{domain}?domain={domain}", sandbox=["allow-forms"]),
            {**frame("/identity?token={secret}"), "name": "secret"},
            {**frame("/identity?owner={owner.name}"), "name": "owner"},
        ],
        "detail_tabs": ["reports.summary"],
    })
    (tmp_path / "summary.js").write_text("export const adminMajor = 1;")
    site.package("reports", tmp_path, tabs={"summary": {"module": "summary.js", "embeddable": True}})
    await site.sync_permissions(db)
    reader = site.test_client(db, user={"key": await grant(db, models, "reader", ["admin.access", "admin.manage_frameitem"])})
    denied = site.test_client(db, user={"key": await grant(db, models, "denied", ["admin.access"])})
    root = site.test_client(db, user={"is_superuser": True})
    root_detail = await root.get(f"{ITEMS}/1")
    assert root_detail.status == 200, root_detail.text
    frames = root_detail.json()["frames"]
    assert urlsplit(frames[0]["url"]).path == "/identity/%C3%A9%20%2F%3F%23%25%26%3D%2B"
    assert urlsplit(frames[0]["url"]).query == urlencode({"domain": SPECIAL})
    assert frames[0]["sandbox"] == "allow-scripts allow-same-origin allow-forms"
    assert frames[0]["referrer_policy"] == "same-origin"
    assert frames[1]["url"] == "/identity?token=Internal"
    assert frames[2]["url"] == "/identity?owner=Private+owner"
    assert (await root.get(f"{ITEMS}/2")).json()["frames"][1]["url"] is None

    detail = await reader.get(f"{ITEMS}/1")
    assert [f["url"] for f in detail.json()["frames"]] == [frames[0]["url"], None, None]
    assert "owner.name" not in [f["label"] for f in detail.json()["fields"]]
    query = urlencode({"domain": SPECIAL})
    response = await reader.get(f"{EMBED}?{query}")
    assert response.status == 200, response.text
    assert response.queries == detail.queries
    body = response.json()
    assert set(body) == {"title", "key", "tab"}
    assert (body["title"], body["key"]) == ("Health", "1")
    assert [f["label"] for f in body["tab"]["fields"]] == ["title"]
    assert body["tab"]["values"] == ["Healthy"]
    assert "Internal" not in (await reader.get(f"{API}/embeds/package?{query}")).text
    package = (await root.get(f"{API}/embeds/package?{query}")).json()["tab"]
    assert package["package"]["tabs"] == {"summary": "packages/reports/summary.js"}
    outside = await reader.get(f"{EMBED}?domain=outside&without=scope")
    missing = await reader.get(f"{EMBED}?domain=missing")
    assert outside.status == missing.status == 404 and outside.json() == missing.json()
    assert (await denied.get(f"{EMBED}?{query}")).status == 403
    assert (await reader.get(f"{EMBED}?domain=one&domain=two")).status == 400
    assert await models["AdminLogEntry"].objects.using(db).count() == 0
    assert await item.objects.using(db).count() == 3
