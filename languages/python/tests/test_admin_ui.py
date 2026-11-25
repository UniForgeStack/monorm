"""The admin UI config of the host (brand, groups, titles) and `admin.url`, on SQLite."""

import datetime

import pytest

import monorm
from monorm import admin

SOURCE = """
datasource db {
  provider = "sqlite"
}

model UiAuthor {
  id   BigInt @id @default(autoincrement())
  name String
  @@map("ui_authors")
}

model UiMember {
  org  String
  slug String
  note String
  @@id([org, slug])
  @@map("ui_members")
}

model UiStamp {
  at DateTime @id
  @@map("ui_stamps")
}

model UiHidden {
  id BigInt @id @default(autoincrement())
  @@map("ui_hidden")
}
"""

SU = {"is_superuser": True}


@pytest.fixture
async def world():
    registry = monorm.Registry()
    classes = monorm.loads(SOURCE, registry=registry)
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield registry, classes, db
    await db.close()


async def test_brand_groups_titles_and_nav_reach_the_resources_route(world):
    registry, classes, db = world
    brand = {"name": "Acme", "colors": {"primary": "#112233"}, "links": [{"label": "Docs", "url": "/docs"}], "environment": "staging"}
    site = admin.Admin(registry, brand=brand, groups=["People", "Other"])
    site.register(classes["UiAuthor"], {"title": "Writers", "group": "People"})
    site.register(classes["UiMember"], {"group": "People"})
    site.register(classes["UiHidden"])
    body = (await site.test_client(db, user=SU).get("/admin/api/resources")).json()
    assert body["brand"]["name"] == "Acme"
    assert body["brand"]["environment"] == "staging"
    assert body["brand"]["links"] == [{"label": "Docs", "url": "/docs"}]
    assert body["nav"] == [
        {"group": None, "resources": ["UiHidden"]},
        {"group": "People", "resources": ["UiAuthor", "UiMember"]},
    ]
    titles = {r["name"]: r["title"] for r in body["resources"]}
    assert titles["UiAuthor"] == "Writers"


async def test_switcher_group_icons_environment_level_and_resource_icon_reach_the_resources_route(world):
    registry, classes, db = world
    switcher = {
        "current": "shop",
        "services": [
            {"key": "shop", "title": "Shop", "icon": "store", "color": "#0a7d5a"},
            {"key": "ops", "title": "Ops", "url": "https://ops.example.com/admin", "icon": "wrench"},
        ],
        "build": {"commit": "0123456789abcdef", "built_at": "2026-11-24T10:00:00Z", "tree": "fedcba9876543210"},
    }
    site = admin.Admin(registry, brand={"environment": "Production"}, groups=["People"], group_icons={"People": "users"}, switcher=switcher)
    site.register(classes["UiAuthor"], {"group": "People", "icon": "pen-line"})
    body = (await site.test_client(db, user=SU).get("/admin/api/resources")).json()
    brand = body["brand"]
    assert brand["environment_level"] == "danger"
    assert brand["colors"]["primary"] == "#0a7d5a"
    assert brand["switcher"]["current"] == "shop"
    assert [s["key"] for s in brand["switcher"]["services"]] == ["shop", "ops"]
    assert brand["switcher"]["services"][1]["url"] == "https://ops.example.com/admin"
    assert brand["switcher"]["build"]["commit"] == "0123456789abcdef"
    assert body["nav"][0]["icon"] == "users"
    assert next(r for r in body["resources"] if r["name"] == "UiAuthor")["icon"] == "pen-line"


async def test_a_wrong_switcher_is_refused(world):
    registry, classes, db = world
    site = admin.Admin(registry, switcher={"current": "missing", "services": [{"key": "shop", "title": "Shop", "url": "/shop"}]})
    site.register(classes["UiAuthor"])
    with pytest.raises(admin.AdminConfigError, match="switcher.current"):
        site.check()
    with pytest.raises(admin.AdminConfigError, match="nope"):
        admin.Admin(registry, switcher={"nope": 1})  # type: ignore[typeddict-unknown-key]
    shop = {"key": "shop", "title": "Shop", "url": "/shop"}
    for options, match in [
        ({"brand": {"environment_level": "info"}}, "needs brand.environment"),
        ({"group_icons": {"Sales": "users"}}, "group_icons"),
        ({"switcher": {"current": "shop", "services": [{**shop, "icon": "Shield!"}]}}, "icon"),
        ({"switcher": {"current": "shop", "services": [shop, {"key": "ops", "title": "Ops", "url": "http://ops.example.com"}]}}, "https"),
        ({"brand": {"colors": {"primary": "#111111"}}, "switcher": {"current": "shop", "services": [{**shop, "color": "#222222"}]}}, "primary"),
    ]:
        bad = admin.Admin(registry, **options)  # type: ignore[arg-type]
        bad.register(classes["UiAuthor"])
        with pytest.raises(admin.AdminConfigError, match=match):
            bad.check()


async def test_a_theme_is_the_short_form_of_the_brand_and_a_value_in_both_is_refused(world):
    registry, classes, db = world
    site = admin.Admin(registry, theme={"title": "Acme", "logo": "/static/acme.svg", "color": "#0a7d5a"})
    site.register(classes["UiAuthor"])
    brand = (await site.test_client(db, user=SU).get("/admin/api/resources")).json()["brand"]
    assert brand["name"] == "Acme"
    assert brand["logo"] == "/static/acme.svg"
    assert brand["colors"]["primary"] == "#0a7d5a"
    clash = admin.Admin(registry, brand={"name": "Other"}, theme={"title": "Acme"})
    clash.register(classes["UiAuthor"])
    with pytest.raises(admin.AdminConfigError, match="theme.title"):
        clash.check()
    with pytest.raises(admin.AdminConfigError, match="shade"):
        admin.Admin(registry, theme={"shade": "x"})  # type: ignore[typeddict-unknown-key]


def test_an_unknown_brand_key_is_refused(world):
    registry, _, _ = world
    with pytest.raises(admin.AdminConfigError, match="nope"):
        admin.Admin(registry, brand={"nope": "x"})  # type: ignore[typeddict-unknown-key]


def test_a_group_that_the_admin_does_not_list_is_refused(world):
    registry, classes, _ = world
    site = admin.Admin(registry, groups=["People"])
    site.register(classes["UiAuthor"], {"group": "Missing"})
    with pytest.raises(admin.AdminConfigError, match="Missing"):
        site.check()


def test_url_takes_a_scalar_a_list_or_an_object(world):
    registry, classes, _ = world
    site = admin.Admin(registry)
    site.register(classes["UiAuthor"])
    site.register(classes["UiMember"])
    assert site.url(classes["UiAuthor"], 7) == "/admin/UiAuthor/7"
    assert site.url(classes["UiMember"], ["acme", "a b"]) == "/admin/UiMember/acme,a%20b"
    assert site.url(classes["UiMember"], ("acme", "x,y")) == "/admin/UiMember/acme,x%2Cy"
    assert site.url(classes["UiMember"], {"slug": "s", "org": "o"}) == "/admin/UiMember/o,s"


def test_url_refuses_a_non_resource_and_a_wrong_key(world):
    registry, classes, _ = world
    site = admin.Admin(registry)
    site.register(classes["UiAuthor"])
    site.register(classes["UiMember"])
    with pytest.raises(admin.AdminConfigError, match="not an admin resource"):
        site.url(classes["UiHidden"], 1)
    with pytest.raises(ValueError):
        site.url(classes["UiMember"], "only-one")
    with pytest.raises(ValueError):
        site.url(classes["UiMember"], {"org": "o", "nope": "s"})


def test_url_sends_a_datetime_key_as_iso_8601_and_refuses_a_value_that_the_route_refuses(world):
    registry, classes, _ = world
    site = admin.Admin(registry)
    site.register(classes["UiStamp"])
    at = datetime.datetime(2026, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    assert site.url(classes["UiStamp"], at) == "/admin/UiStamp/2026-01-02T03%3A04%3A05%2B00%3A00"
    with pytest.raises(ValueError):
        site.url(classes["UiStamp"], "2026-01-02 03:04:05")


async def test_the_url_of_a_row_opens_in_the_detail_route(world):
    registry, classes, db = world
    site = admin.Admin(registry)
    site.register(classes["UiMember"])
    await classes["UiMember"].objects.using(db).insert_many([{"org": "a/b", "slug": "c,d", "note": "hi"}])
    path = site.url(classes["UiMember"], ["a/b", "c,d"])
    api = path.replace("/admin/", "/admin/api/resources/", 1)
    reply = await site.test_client(db, user=SU).get(api)
    assert reply.status == 200
    assert reply.json()["values"][-1] == "hi"

