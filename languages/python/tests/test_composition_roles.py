"""Column roles (updated_at, soft delete, version) on the root of a composed model."""

import os
from datetime import datetime, timezone

import pytest

import monorm
from artifact_caps import has_extension

NEEDS = ("monorm.composition", "monorm.timestamps", "monorm.soft-delete", "monorm.locking")
pytestmark = pytest.mark.skipif(not has_extension(*NEEDS), reason="requires the composition, timestamps, soft-delete and locking packages")

OLD = datetime(2000, 1, 1, tzinfo=timezone.utc)
DATABASES = ["sqlite", "postgres"]

TABLES = ("comproles_people", "comproles_employees", "comproles_managers")
ROLES = """
model Person {{
 id Int @id @default(autoincrement())
 name String
 @@timestamps.updated_at("changed", mode: "{mode}")
 @@soft_delete.deleted_at("deleted", mode: "{mode}")
 @@locking.version("version")
 @@map("comproles_people")
}}
model Employee {{
 salary Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("comproles_employees")
}}
model Manager {{
 level Int
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
 @@map("comproles_managers")
}}
"""


async def connect(dialect: str, source: str) -> tuple[monorm.Database, dict]:
    if dialect == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + source
    registry = monorm.Registry()
    models = monorm.loads(source, registry=registry)
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, models


def as_utc(value: datetime) -> datetime:
    return value if value.tzinfo else value.replace(tzinfo=timezone.utc)


async def rows(db, table: str) -> int:
    return len(await db.fetch(f"SELECT * FROM {table}"))


@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_a_change_on_any_level_bumps_the_root(dialect, mode):
    db, models = await connect(dialect, ROLES.format(mode=mode))
    Manager = models["Manager"]
    try:
        managers = Manager.objects.using(db)
        alice = await managers.insert(name="Alice", salary=1, level=1, changed=OLD)
        assert alice.changed == OLD
        before = alice.version
        # A change on the lowest level alone still bumps the root.
        assert await managers.filter(Manager.id == alice.id).update(level=2) == 1
        alice = await managers.get(Manager.id == alice.id)
        assert alice.level == 2
        assert as_utc(alice.changed) > OLD
        assert alice.version == before + 1
        await db.execute("UPDATE comproles_people SET changed = '2000-01-01 00:00:00' WHERE id = 1")
        await db.execute("UPDATE comproles_employees SET salary = 9 WHERE id = 1")
        alice = await managers.get(Manager.id == alice.id)
        assert (as_utc(alice.changed) > OLD) is (mode == "database")
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_a_soft_delete_of_a_composed_row_updates_only_the_root(dialect, mode):
    db, models = await connect(dialect, ROLES.format(mode=mode))
    Person, Manager = models["Person"], models["Manager"]
    try:
        managers = Manager.objects.using(db)
        await managers.insert_many([{"name": "Alice", "salary": 1, "level": 1}, {"name": "Bob", "salary": 2, "level": 2}])
        assert await managers.filter(Manager.name == "Alice").delete() == 1
        assert [await rows(db, t) for t in ("comproles_people", "comproles_employees", "comproles_managers")] == [2, 2, 2]
        assert [m.name for m in await managers] == ["Bob"]
        assert [p.name for p in await Person.objects.using(db)] == ["Bob"]
        assert [m.name for m in await managers.deleted_only()] == ["Alice"]
        assert await managers.filter(Manager.name == "Alice").delete() == 0
        assert await managers.filter(Manager.name == "Alice").undelete() == 1
        assert sorted(m.name for m in await Manager.objects.using(db)) == ["Alice", "Bob"]
        # A hard delete of a soft-deleted row removes it from every level; the root cascades to the levels.
        assert await managers.filter(Manager.name == "Bob").delete() == 1
        await Person.objects.using(db).filter(Person.name == "Bob").hard_delete()
        assert [await rows(db, t) for t in ("comproles_people", "comproles_employees", "comproles_managers")] == [1, 1, 1]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_raw_delete_on_the_root_table_soft_deletes_in_database_mode(dialect):
    db, models = await connect(dialect, ROLES.format(mode="database"))
    Manager = models["Manager"]
    try:
        await Manager.objects.using(db).insert(name="Alice", salary=1, level=1)
        await db.execute("DELETE FROM comproles_people WHERE id = 1")
        assert [await rows(db, t) for t in ("comproles_people", "comproles_employees", "comproles_managers")] == [1, 1, 1]
        assert await Manager.objects.using(db).count() == 0
        assert await Manager.objects.using(db).deleted_only().count() == 1
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("attribute", ['@@timestamps.updated_at("changed")', '@@soft_delete.deleted_at("deleted")', "@@locking.version"])
def test_a_role_below_the_root_is_an_error_that_names_the_root(attribute):
    source = f"""
model Person {{
 id Int @id
 name String
 @@map("comproles_below_people")
}}
model Employee {{
 salary Int
 {attribute}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("comproles_below_employees")
}}
"""
    with pytest.raises(Exception, match="declare it on Person"):
        monorm.loads('datasource db {\n provider = "sqlite"\n}\n' + source, registry=monorm.Registry())


async def watch(db):
    events: list = []
    remove = db.on_query(events.append)
    return events, remove


@pytest.mark.parametrize("mode", ["database", "application"])
async def test_a_bulk_insert_on_postgres_costs_a_key_statement_one_insert_per_level_and_a_read(mode):
    db, models = await connect("postgres", ROLES.format(mode=mode))
    Manager = models["Manager"]
    try:
        managers = Manager.objects.using(db)
        events, remove = await watch(db)
        try:
            made = await managers.insert_many([{"name": f"m{i}", "salary": i, "level": i * 10} for i in range(5)]).returning()
        finally:
            remove()
        assert [m.name for m in made] == [f"m{i}" for i in range(5)]
        assert [m.level for m in made] == [0, 10, 20, 30, 40]
        assert len({m.id for m in made}) == 5
        assert all(m.version == 0 and m.deleted is None for m in made)
        sqls = [e.sql.lower() for e in events]
        assert len(sqls) == 5, sqls  # key, one insert for each of three levels, one read
        assert "generate_series" in sqls[0] and "nextval" in sqls[0]
        assert all(s.startswith("insert into") for s in sqls[1:4]), sqls
        assert sqls[4].startswith("select")
        assert [await rows(db, t) for t in ("comproles_people", "comproles_employees", "comproles_managers")] == [5, 5, 5]
    finally:
        await db.close()


async def test_a_bulk_insert_with_caller_keys_skips_the_key_statement_and_keeps_input_order():
    db, models = await connect("postgres", ROLES.format(mode="database"))
    Manager = models["Manager"]
    try:
        managers = Manager.objects.using(db)
        events, remove = await watch(db)
        try:
            made = await managers.insert_many([{"id": k, "name": f"m{k}", "salary": 1, "level": 1} for k in (30, 10, 20)]).returning()
        finally:
            remove()
        assert [m.id for m in made] == [30, 10, 20]
        assert len(events) == 4  # three inserts and one read
        # A mix: the rows without a key take new keys in one statement.
        events, remove = await watch(db)
        try:
            mixed = await managers.insert_many([{"name": "a", "salary": 1, "level": 1}, {"id": 99, "name": "b", "salary": 1, "level": 1}, {"name": "c", "salary": 1, "level": 1}]).returning()
        finally:
            remove()
        assert [m.name for m in mixed] == ["a", "b", "c"]
        assert mixed[1].id == 99 and mixed[0].id != mixed[2].id
        assert len(events) == 5
    finally:
        await db.close()


async def test_a_bulk_insert_on_sqlite_stays_row_by_row():
    db, models = await connect("sqlite", ROLES.format(mode="database"))
    Manager = models["Manager"]
    try:
        managers = Manager.objects.using(db)
        events, remove = await watch(db)
        try:
            made = await managers.insert_many([{"name": f"m{i}", "salary": i, "level": i} for i in range(3)]).returning()
        finally:
            remove()
        assert [m.name for m in made] == ["m0", "m1", "m2"]
        assert len(events) >= 9  # at least one insert for each level and row
    finally:
        await db.close()


async def test_a_bulk_insert_with_copy_keeps_its_error():
    db, models = await connect("postgres", ROLES.format(mode="database"))
    Manager = models["Manager"]
    try:
        with pytest.raises(Exception, match="(?i)copy"):
            await Manager.objects.using(db).insert_many([{"name": "a", "salary": 1, "level": 1}], copy=True)
    finally:
        await db.close()


@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_bulk_attach_adds_the_child_levels_to_existing_parents_in_input_order(dialect):
    db, models = await connect(dialect, ROLES.format(mode="database"))
    Person, Employee = models["Person"], models["Employee"]
    try:
        people = await Person.objects.using(db).insert_many([{"name": f"p{i}"} for i in range(3)]).returning()
        ids = [p.id for p in people]
        events, remove = await watch(db)
        try:
            made = await Employee.objects.using(db).attach_many([(ids[2], {"salary": 30}), (ids[0], {"salary": 10}), (ids[1], {"salary": 20})])
        finally:
            remove()
        assert [(e.id, e.salary, e.name) for e in made] == [(ids[2], 30, "p2"), (ids[0], 10, "p0"), (ids[1], 20, "p1")]
        assert len(events) == 2  # one insert for the level and one read
        assert await rows(db, "comproles_employees") == 3
    finally:
        await db.close()


CASCADE = """
model Shop {
 id Int @id @default(autoincrement())
 @@soft_delete.deleted_at("deleted", mode: "database")
 @@map("comcas_shops")
}
model Person {
 id Int @id @default(autoincrement())
 name String
 @@soft_delete.deleted_at("deleted", mode: "database")
 @@map("comcas_people")
}
model Employee {
 shop_id Int
 shop Shop @relation(fields: [shop_id], references: [id], onDelete: Cascade)
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("comcas_employees")
}
"""


@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_soft_delete_cascades_to_a_composed_child_through_its_root(dialect):
    db, models = await connect(dialect, CASCADE)
    Shop, Employee = models["Shop"], models["Employee"]
    try:
        shops = await Shop.objects.using(db).insert_many([{}, {}]).returning()
        for shop, names in ((shops[0], ["a", "b"]), (shops[1], ["c"])):
            for name in names:
                await Employee.objects.using(db).insert(name=name, shop_id=shop.id)
        assert await Shop.objects.using(db).filter(Shop.id == shops[0].id).delete() == 1
        assert sorted(e.name for e in await Employee.objects.using(db)) == ["c"]
        # The rows stay, the root carries the flag, and every level row is still there.
        assert await rows(db, "comcas_people") == 3
        assert await rows(db, "comcas_employees") == 3
        flagged = await db.fetch("SELECT name FROM comcas_people WHERE deleted IS NOT NULL ORDER BY name")
        assert [r["name"] for r in flagged] == ["a", "b"]
    finally:
        await db.close()


@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_a_hard_delete_of_a_live_composed_row_removes_every_level(dialect, mode):
    db, models = await connect(dialect, ROLES.format(mode=mode))
    Manager, Person = models["Manager"], models["Person"]
    try:
        managers = Manager.objects.using(db)
        await managers.insert_many([{"name": "Alice", "salary": 1, "level": 1}, {"name": "Bob", "salary": 2, "level": 2}])
        alice = Person.objects.using(db).filter(Person.name == "Alice")
        await alice.hard_delete()
        # In database mode the root trigger soft-deletes a live row; a second hard delete removes it.
        assert [await rows(db, t) for t in TABLES] == ([2, 2, 2] if mode == "database" else [1, 1, 1])
        await alice.hard_delete()
        assert [await rows(db, t) for t in TABLES] == [1, 1, 1]
        assert [m.name for m in await managers.all_with_deleted()] == ["Bob"]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_a_stale_write_through_a_child_model_raises_version_conflict(dialect, mode):
    db, models = await connect(dialect, ROLES.format(mode=mode))
    Manager = models["Manager"]
    try:
        managers = Manager.objects.using(db)
        first = await managers.insert(name="Alice", salary=1, level=1)
        second = await managers.get(Manager.id == first.id)
        await first.update(level=2)
        assert first.version == second.version + 1
        with pytest.raises(monorm.VersionConflict):
            await second.update(level=3)
        assert (await managers.get(Manager.id == first.id)).level == 2
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_a_stale_delete_through_a_child_model_raises_version_conflict(dialect, mode):
    db, models = await connect(dialect, ROLES.format(mode=mode))
    Manager = models["Manager"]
    try:
        managers = Manager.objects.using(db)
        first = await managers.insert(name="Alice", salary=1, level=1)
        second = await managers.get(Manager.id == first.id)
        await first.update(level=2)
        with pytest.raises(monorm.VersionConflict):
            await second.delete()
        assert await managers.count() == 1
    finally:
        await db.drop_tables()
        await db.close()
