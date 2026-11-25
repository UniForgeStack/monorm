"""The code of an admin error and its class: `AdminConfigError` for the site config, `AdminSettingsError` for the settings."""

import pytest

import monorm
from monorm import admin
from monorm_auth import UserMethods

SECRET = "s" * 40
SCHEMA = """datasource db {
  provider = "sqlite"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")
"""


@pytest.fixture
async def world(tmp_path):
    (tmp_path / "schema.prisma").write_text(SCHEMA)
    registry = monorm.Registry()
    models = monorm.load(tmp_path / "schema.prisma", registry=registry, model_methods={"User": UserMethods})
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    yield admin.Admin(registry), db, models
    await db.close()


async def test_a_wrong_config_raises_a_config_error_with_a_code(world):
    site, _, models = world
    site.register(models["Role"], {"list": {"page_size": 0}})
    with pytest.raises(admin.AdminConfigError, match="Role: list.page_size is 0") as e:
        site.check()
    assert not isinstance(e.value, admin.AdminSettingsError)
    assert e.value.code == "config.invalid_value"


async def test_a_wrong_secret_raises_a_settings_error_with_a_code(world):
    site, db, _ = world
    with pytest.raises(admin.AdminSettingsError) as e:
        await site.test_client(db, secret="short").get("/admin/api/me")
    assert e.value.code.startswith("settings.")
    assert isinstance(e.value, admin.AdminConfigError)


async def test_a_settings_key_that_does_not_exist_has_its_own_code(world):
    site, db, _ = world
    with pytest.raises(admin.AdminSettingsError) as e:
        await site.test_client(db, secret=SECRET, settings={"nope": 1}).get("/admin/api/me")
    assert e.value.code == "settings.unknown_key"
