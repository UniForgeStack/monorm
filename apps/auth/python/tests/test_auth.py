import base64
import hashlib
from pathlib import Path
from typing import Any

import pytest

import monorm_auth
from monorm_auth import authenticate, check_password, make_password, needs_update, sync_permissions

from auth_support import Auth

APP = Path(__file__).parents[2]


def django_hash(raw: str) -> str:
    return (
        "pbkdf2_sha256$1000$NaCl$"
        + base64.b64encode(hashlib.pbkdf2_hmac("sha256", raw.encode(), b"NaCl", 1000)).decode()
    )


def test_the_schema_copies_of_both_packages_match_the_source() -> None:
    source = APP.joinpath("schema.prisma").read_text()
    assert APP.joinpath("python/monorm_auth/schema.prisma").read_text() == source
    assert APP.joinpath("../../languages/js/auth/schema.prisma").read_text() == source


async def test_the_models_get_the_methods_and_the_query_set(auth: Auth) -> None:
    assert isinstance(auth.User.objects, monorm_auth.UserQueries)
    users = await auth.seed()
    assert isinstance(users["staff"], monorm_auth.UserMethods)
    assert auth.User.__name__ == auth.variant.model


async def test_set_password_hashes_the_snapshot_and_update_writes_it(auth: Auth) -> None:
    users = await auth.seed()
    user = users["direct"]
    user.set_password("s3cret")
    assert user.check_password("s3cret") and not user.check_password("nope")
    assert (await auth.User.objects.using(auth.db).get(auth.User.id == user.id)).password == "!"
    await user.update(password=user.password)
    assert check_password("s3cret", (await auth.User.objects.using(auth.db).get(auth.User.id == user.id)).password)


async def test_check_password_with_update_rehashes_a_django_hash_but_not_a_wrong_password(auth: Auth) -> None:
    (user,) = (
        await auth.User.objects.using(auth.db)
        .insert_many([{"username": "dj", "email": "d@x.y", "password": django_hash("s3cret")}])
        .returning()
    )
    assert not user.check_password("wrong", update=True) and user.password.startswith("pbkdf2_sha256$")
    assert user.check_password("s3cret") and user.password.startswith("pbkdf2_sha256$")
    assert user.check_password("s3cret", update=True)
    assert not needs_update(user.password) and check_password("s3cret", user.password)


async def test_a_missing_user_still_costs_one_hash_check(auth: Auth, monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[str] = []
    real = monorm_auth.auth.check_password
    monkeypatch.setattr(monorm_auth.auth, "check_password", lambda raw, encoded: seen.append(encoded) or real(raw, encoded))
    assert await authenticate("nobody", "s3cret", **auth.kw) is None
    assert seen == [monorm_auth.auth._DUMMY]


async def test_authenticate(auth: Auth) -> None:
    kw = auth.kw
    await auth.User.objects.using(auth.db).insert_many(
        [
            {"username": "ann", "email": "a@x.y", "password": make_password("s3cret")},
            {"username": "dj", "email": "d@x.y", "password": django_hash("s3cret")},
            {"username": "off", "email": "o@x.y", "password": make_password("s3cret"), "is_active": False},
        ]
    )
    ann = await authenticate("ann", "s3cret", **kw)
    assert ann is not None and ann.username == "ann"
    assert await authenticate("ann", "wrong", **kw) is None
    assert await authenticate("nobody", "s3cret", **kw) is None
    assert await authenticate("off", "s3cret", **kw) is None
    dj = await authenticate("dj", "s3cret", **kw)
    assert dj is not None and not needs_update(dj.password)
    stored = await auth.User.objects.using(auth.db).get(auth.User.username == "dj")
    assert stored.password == dj.password and check_password("s3cret", stored.password)


async def test_authenticate_takes_the_user_model(auth: Auth) -> None:
    await auth.User.objects.using(auth.db).insert_many(
        [{"username": "ann", "email": "a@x.y", "password": make_password("s3cret")}]
    )
    ann = await authenticate("ann", "s3cret", using=auth.db, model=auth.User)
    assert ann is not None and type(ann) is auth.User


async def test_a_wrong_password_does_not_rehash(auth: Auth) -> None:
    old = django_hash("s3cret")
    await auth.User.objects.using(auth.db).insert_many([{"username": "dj", "email": "d@x.y", "password": old}])
    assert await authenticate("dj", "wrong", **auth.kw) is None
    assert (await auth.User.objects.using(auth.db).get(auth.User.username == "dj")).password == old


async def test_has_perm_and_has_perms(auth: Auth) -> None:
    u = await auth.seed()
    assert await u["staff"].has_perm("blog.add_post")
    assert not await u["staff"].has_perm("blog.change_post")
    assert not await u["staff"].has_perm("shop.add_post")
    assert await u["direct"].has_perm("blog.change_post") and not await u["direct"].has_perm("blog.add_post")
    assert await u["root"].has_perm("anything.at_all")
    assert not await u["idle"].has_perm("blog.add_post") and not await u["idle"].has_perm("anything.at_all")
    assert await u["staff"].has_perms(["blog.add_post"]) and not await u["staff"].has_perms(
        ["blog.add_post", "blog.change_post"]
    )
    assert await u["staff"].has_perms([])


async def test_has_perm_rejects_a_name_without_a_label(auth: Auth) -> None:
    u = await auth.seed()
    with pytest.raises(ValueError, match="<label>.<codename>"):
        await u["staff"].has_perm("add_post")


async def test_get_all_permissions(auth: Auth) -> None:
    u = await auth.seed()
    assert await u["staff"].get_all_permissions() == {"blog.add_post"}
    assert await u["direct"].get_all_permissions() == {"blog.change_post"}
    assert await u["root"].get_all_permissions() == {"blog.add_post", "blog.change_post", "shop.add_post"}
    assert await u["idle"].get_all_permissions() == set()


async def test_with_perm(auth: Auth) -> None:
    await auth.seed()
    names = lambda qs: sorted(x.username for x in qs)  # noqa: E731
    users = auth.User.objects.using(auth.db)
    assert names(await users.with_perm("blog.add_post")) == ["root", "staff"]
    assert names(await users.with_perm("blog.change_post")) == ["direct", "root"]
    assert names(await users.with_perm("nothing.here")) == ["root"]
    assert names(await users.filter(auth.User.username != "root").with_perm("blog.add_post")) == ["staff"]


async def test_sync_permissions_adds_the_missing_rows_once(auth: Auth) -> None:
    class Post: ...

    class Comment: ...

    kw: dict[str, Any] = {"using": auth.db, "registry": auth.registry}
    assert await sync_permissions([Post], "blog", **kw) == 4
    assert await sync_permissions([Post, Comment], "blog", **kw) == 4
    assert await sync_permissions([Post, Comment], "blog", **kw) == 0
    assert await sync_permissions([], "blog", **kw) == 0
    rows = await auth.Permission.objects.using(auth.db).filter(auth.Permission.label == "blog").all()
    assert sorted(r.codename for r in rows) == sorted(
        f"{a}_{n}" for a in ("add", "change", "delete", "view") for n in ("post", "comment")
    )
    assert {r.name for r in rows if r.codename == "add_post"} == {"Can add post"}
