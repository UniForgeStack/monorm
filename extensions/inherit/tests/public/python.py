"""`model_for` and the expanded models from the schema language to the database; needs the `orm-inherit` package."""

import asyncio
from pathlib import Path

import orm
from orm_inherit import model_for

ROOT = Path(__file__).parent.parent
FIXTURE = ROOT.joinpath("fixtures/schema.prisma").read_text()


async def run(provider: str, url: str) -> None:
    registry = orm.Registry()
    models = orm.loads(FIXTURE.replace('"sqlite"', f'"{provider}"'), registry=registry)
    assert sorted(models) == ["Admin", "AdminTeam", "MyUser", "Role", "Team", "UserTeam"], sorted(models)
    MyUser, Admin, Role = models["MyUser"], models["Admin"], models["Role"]
    Team, UserTeam, AdminTeam = models["Team"], models["UserTeam"], models["AdminTeam"]
    assert model_for("User", registry) is MyUser
    assert model_for("Admin", registry) is Admin
    assert model_for("Role", registry) is Role
    assert list(MyUser._meta.fields)[:4] == ["id", "email", "created_at", "bio"]
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        User = model_for("User", registry)
        (user,) = await User.objects.using(db).insert_many([{"email": "a@b.c", "bio": "hi"}]).returning()
        (admin,) = await Admin.objects.using(db).insert_many([{"email": "x@y.z"}]).returning()
        assert user.email == "a@b.c" and user.bio == "hi" and user.created_at is not None
        assert admin.level == 1
        await Role.objects.using(db).insert_many(
            [{"name": "staff", "user_roles_id": user.pk}, {"name": "root", "admin_roles_id": admin.pk}]
        )
        assert [r.name for r in await user.roles.using(db).all()] == ["staff"]
        assert [r.name for r in await admin.roles.using(db).all()] == ["root"]
        teams = await Team.objects.using(db).insert_many([{"name": "ops"}, {"name": "dev"}, {"name": "qa"}]).returning()
        await UserTeam.objects.using(db).insert_many(
            [{"user_id": user.pk, "team_id": teams[0].pk}, {"user_id": user.pk, "team_id": teams[1].pk}]
        )
        await AdminTeam.objects.using(db).insert_many([{"admin_id": admin.pk, "team_id": teams[2].pk}])
        assert sorted(t.name for t in await user.teams.using(db).all()) == ["dev", "ops"]
        assert [t.name for t in await admin.teams.using(db).all()] == ["qa"]
        assert [u.email for u in await teams[0].user_teams.using(db).all()] == ["a@b.c"]
        assert [a.email for a in await teams[2].admin_teams.using(db).all()] == ["x@y.z"]
    finally:
        await db.close()


asyncio.run(run("sqlite", "sqlite://:memory:"))
print("inherit python ok")
