"""Model composition through the public API; needs the monorm-composition package."""

import os

import pytest
import monorm
from artifact_caps import has_extension

pytestmark = pytest.mark.skipif(not has_extension("monorm.composition"), reason="requires the monorm.composition package")

SOURCE = """
model Person {
 id Int @id @default(autoincrement())
 name String @check("name <> 'BADROOT'")
 @@map("composition06_people")
}
model Employee {
 salary Int @check("salary > 0")
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("composition06_employees")
}
model Manager {
 level Int @check("level > 0")
 @@composition.model(parent: "Employee", parentRef: "employee", childRef: "manager")
 @@map("composition06_managers")
}
model Customer {
 points Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "customer")
 @@map("composition06_customers")
}
"""


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_composed_create_attach_update_and_delete(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SOURCE, registry=registry)
    Person, Employee, Manager, Customer = (models[k] for k in ("Person", "Employee", "Manager", "Customer"))
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        insert = Manager.objects.using(db).insert(name="Alice", salary=20, level=1)
        result = insert.inspect()
        assert result.statement_count == 4  # three owners and the logical identity read
        assert not result.has_batching
        assert result.statements[-1] == monorm.StatementInspection(joins_count=1, scalar_correlated=2)
        alice = await insert
        assert (alice.name, alice.salary, alice.level) == ("Alice", 20, 1)
        counts = [await m.objects.using(db).count() for m in (Person, Employee, Manager)]
        assert counts == [1, 1, 1]

        # One person is an Employee and a Customer at the same time.
        customer = await Customer.objects.using(db).attach(alice.id, {"points": 5})
        assert (customer.id, customer.name, customer.points) == (alice.id, "Alice", 5)
        with pytest.raises(monorm.IntegrityError):
            await Customer.objects.using(db).attach(alice.id, {"points": 6})
        with pytest.raises(monorm.IntegrityError):
            await Customer.objects.using(db).attach(alice.id + 100, {"points": 6})
        assert await Customer.objects.using(db).count() == 1

        # A failed leaf insert rolls back its root and middle rows.
        with pytest.raises(monorm.IntegrityError):
            await Manager.objects.using(db).insert(name="Bob", salary=10, level=0)
        with pytest.raises(monorm.IntegrityError):
            await Manager.objects.using(db).insert(name="BADROOT", salary=10, level=1)
        assert await Person.objects.using(db).count() == 1

        # An inherited-field filter selects the composed rows to update.
        update = Manager.objects.using(db).filter(Manager.salary == 20).update(level=4)
        result = update.inspect()
        assert result.statement_count == 2  # capture identities, then repeat the owner update
        assert result.has_batching
        assert await update == 1
        assert await Manager.objects.using(db).filter(Manager.salary == 99).update(level=5) == 0
        assert (await Manager.objects.using(db).get(Manager.id == alice.id)).level == 4
        assert (await Employee.objects.using(db).filter(Employee.name == "Alice").get()).salary == 20

        person = await Person.objects.using(db).load(Person.employee, Person.customer).get()
        assert (person.employee.salary, person.customer.points) == (20, 5)

        # Deleting through a child removes every level, the sibling child too.
        delete = Manager.objects.using(db).filter(Manager.id == alice.id).delete()
        assert delete.inspect().statement_count == 2
        assert delete.inspect().has_batching
        await delete
        counts = [await m.objects.using(db).count() for m in (Person, Employee, Manager, Customer)]
        assert counts == [0, 0, 0, 0]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_delete_through_a_child_model_removes_every_level_and_counts_like_the_root(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SOURCE, registry=registry)
    Person, Employee, Manager = (models[k] for k in ("Person", "Employee", "Manager"))
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        managers = Manager.objects.using(db)
        await managers.insert_many([{"name": "Alice", "salary": 1, "level": 1}, {"name": "Bob", "salary": 2, "level": 2}, {"name": "Cy", "salary": 3, "level": 3}, {"name": "Dee", "salary": 4, "level": 4}])
        assert await managers.filter(Manager.name == "Alice").delete() == 1
        returned = await managers.filter(Manager.name == "Bob").delete().returning()
        assert [m.name for m in returned] == ["Bob"]
        people = Person.objects.using(db)
        assert await people.filter(Person.name == "Cy").delete() == 1
        returned = await people.filter(Person.name == "Dee").delete().returning()
        assert [p.name for p in returned] == ["Dee"]
        assert [await m.objects.using(db).count() for m in (Person, Employee, Manager)] == [0, 0, 0]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_delete_through_a_child_model_over_tables_without_a_cascade_fails_and_deletes_nothing(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SOURCE, registry=registry)
    Person, Employee, Manager = (models[k] for k in ("Person", "Employee", "Manager"))
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    # Tables made by hand: the manager key has no ON DELETE CASCADE.
    for statement in (
        "CREATE TABLE composition06_people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE composition06_employees (id INTEGER PRIMARY KEY REFERENCES composition06_people (id) ON DELETE CASCADE, salary INTEGER NOT NULL)",
        "CREATE TABLE composition06_managers (id INTEGER PRIMARY KEY REFERENCES composition06_employees (id), level INTEGER NOT NULL)",
        "CREATE TABLE composition06_customers (id INTEGER PRIMARY KEY, points INTEGER NOT NULL)",
        "INSERT INTO composition06_people (id, name) VALUES (1, 'Alice')",
        "INSERT INTO composition06_employees (id, salary) VALUES (1, 5)",
        "INSERT INTO composition06_managers (id, level) VALUES (1, 1)",
    ):
        await db.execute(statement)
    try:
        with pytest.raises(monorm.QueryError, match="ON DELETE CASCADE.*composition06_managers.id"):
            await Manager.objects.using(db).filter(Manager.name == "Alice").delete()
        assert [await m.objects.using(db).count() for m in (Person, Employee, Manager)] == [1, 1, 1]
    finally:
        for table in ("managers", "employees", "customers", "people"):
            await db.execute(f"DROP TABLE composition06_{table}")
        await db.close()


async def _hand_made_tables(db, manager_key):
    """The composition06 tables, empty, with `manager_key` as the manager level key."""
    await db.drop_tables()
    for statement in (
        "CREATE TABLE composition06_people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE composition06_employees (id INTEGER PRIMARY KEY REFERENCES composition06_people (id) ON DELETE CASCADE, salary INTEGER NOT NULL)",
        f"CREATE TABLE composition06_managers ({manager_key}, level INTEGER NOT NULL)",
        "CREATE TABLE composition06_customers (id INTEGER PRIMARY KEY REFERENCES composition06_people (id) ON DELETE CASCADE, points INTEGER NOT NULL)",
    ):
        await db.execute(statement)


async def _drop_hand_made_tables(db):
    for table in ("managers", "employees", "customers", "people"):
        await db.execute(f"DROP TABLE IF EXISTS composition06_{table}")


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_composed_delete_checks_the_cascades_again_after_a_script_changes_them(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SOURCE, registry=registry)
    Manager = models["Manager"]
    db = await monorm.connect(url, registry=registry, default=False)
    plain = "id INTEGER PRIMARY KEY REFERENCES composition06_employees (id)"
    try:
        await _hand_made_tables(db, plain)
        with pytest.raises(monorm.QueryError, match="composition06_managers.id"):
            await Manager.objects.using(db).filter(Manager.name == "Alice").delete()
        # The failure was not cached: the next delete sees the cascade that was added.
        await _hand_made_tables(db, plain + " ON DELETE CASCADE")
        await Manager.objects.using(db).filter(Manager.name == "Alice").delete()
        # A script that drops the cascade clears the passed check, so the next delete fails again.
        await _hand_made_tables(db, plain)
        with pytest.raises(monorm.QueryError, match="composition06_managers.id"):
            await Manager.objects.using(db).filter(Manager.name == "Alice").delete()
        async with db.transaction():
            with pytest.raises(monorm.QueryError, match="composition06_managers.id"):
                await Manager.objects.using(db).filter(Manager.name == "Alice").delete()
    finally:
        await _drop_hand_made_tables(db)
        await db.close()


async def test_a_composed_delete_compares_key_names_without_case_and_needs_foreign_keys_on_sqlite():
    registry = monorm.Registry()
    models = monorm.loads('datasource db {\n provider = "sqlite"\n}\n' + SOURCE, registry=registry)
    Manager = models["Manager"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await _hand_made_tables(db, "ID INTEGER PRIMARY KEY REFERENCES COMPOSITION06_EMPLOYEES (ID) ON DELETE CASCADE")
        await Manager.objects.using(db).filter(Manager.name == "Alice").delete()
    finally:
        await _drop_hand_made_tables(db)
        await db.close()
    off = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await _hand_made_tables(off, "id INTEGER PRIMARY KEY REFERENCES composition06_employees (id) ON DELETE CASCADE")
        await off.execute("PRAGMA foreign_keys = OFF")
        with pytest.raises(monorm.QueryError, match="PRAGMA foreign_keys = ON"):
            await Manager.objects.using(off).filter(Manager.name == "Alice").delete()
    finally:
        await _drop_hand_made_tables(off)
        await off.close()


@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_a_failed_composed_insert_marks_the_enclosing_transaction(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SOURCE, registry=registry)
    Person, Manager = models["Person"], models["Manager"]
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        bad = {"name": "Bob", "salary": 10, "level": 0}
        # Outside a transaction the insert is atomic: its root row rolls back.
        with pytest.raises(monorm.IntegrityError):
            await Manager.objects.using(db).insert(**bad)
        assert await Person.objects.using(db).count() == 0
        # Inside one it opens no savepoint: the failure marks the transaction.
        with pytest.raises(monorm.TransactionAborted) as e:
            async with db.transaction():
                with pytest.raises(monorm.IntegrityError):
                    await Manager.objects.using(db).insert(**bad)
        assert isinstance(e.value.__cause__, monorm.IntegrityError)
        assert await Person.objects.using(db).count() == 0
        # A nested transaction() recovers.
        async with db.transaction():
            with pytest.raises(monorm.IntegrityError):
                async with db.transaction():
                    await Manager.objects.using(db).insert(**bad)
            await Manager.objects.using(db).insert(name="Ok", salary=1, level=1)
        assert await Person.objects.using(db).count() == 1
    finally:
        await db.drop_tables()
        await db.close()


SCOPED = """
model Person {
 id Int @id @default(autoincrement())
 name String
 @@map("composition06_scoped_people")
}
model Employee {
 shop_id Int
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@query.filter("tenant", shop_id == scope.shop)
 @@map("composition06_scoped_employees")
}
"""


@pytest.mark.skipif(
    not has_extension("monorm.query-defaults"),
    reason="requires the monorm.query-defaults package",
)
@pytest.mark.parametrize("provider", ["sqlite", "postgresql"])
async def test_composed_writes_read_the_scope(provider):
    url = "sqlite://:memory:" if provider == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL")
    if not url:
        pytest.skip("set ORM_TEST_DATABASE_URL for PostgreSQL")
    registry = monorm.Registry()
    models = monorm.loads(f'datasource db {{\n provider = "{provider}"\n}}\n' + SCOPED, registry=registry)
    Employee = models["Employee"]
    db = await monorm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        employees = Employee.objects.using(db)
        with monorm.scope(shop=1):
            ann = await employees.insert(name="Ann", shop_id=1)
            assert (ann.name, ann.shop_id) == ("Ann", 1)
        with monorm.scope(shop=2):
            await employees.insert(name="Bob", shop_id=2)
            # Inserts do not read the scope, and the identity read skips the filter.
            assert (await employees.insert(name="Cid", shop_id=1)).name == "Cid"
        with monorm.scope(shop=1):
            assert await employees.filter(Employee.name == "Ann").update(name="Ann 2") == 1
            assert await employees.filter(Employee.name == "Bob").delete() == 0
        assert await employees.without_defaults().filter(Employee.name == "Bob").update(name="Bob 2") == 1
        assert sorted(e.name for e in await employees.without_defaults()) == ["Ann 2", "Bob 2", "Cid"]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize(
    "attribute,extension",
    [
        ('@@locking.version', "monorm.locking"),
        ('@@soft_delete.deleted_at("deleted")', "monorm.soft-delete"),
        ('@@timestamps.updated_at("changed")', "monorm.timestamps"),
    ],
)
def test_a_tracking_attribute_below_the_root_of_a_composed_model_is_rejected(attribute, extension):
    if not has_extension(extension):
        pytest.skip(f"requires the {extension} extension")
    source = f"""
model Person {{
 id Int @id
 name String
 @@map("composition06_role_people")
}}
model Employee {{
 salary Int
 {attribute}
 @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
 @@map("composition06_role_employees")
}}
"""
    with pytest.raises(Exception, match="below the root of its ownership chain; declare it on Person"):
        monorm.loads('datasource db {\n provider = "sqlite"\n}\n' + source, registry=monorm.Registry())


COMPUTED = """
model Person {
 id Int @id @default(autoincrement())
 name String
 @@map("composition_computed_people")
}
model Staff {
 text String
 start Int
 end Int
 shop_id Int
 updated_by String?
 first String
 last String
 @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")
 @@outside.guard
 @@map("composition_computed_staff")
}
"""


@pytest.mark.skipif(not has_extension("acme.outside"), reason="requires the acme.outside test extension")
async def test_a_composed_write_returns_the_computed_field():
    registry = monorm.Registry()
    models = monorm.loads('datasource db {\n provider = "sqlite"\n}\n' + COMPUTED, registry=registry)
    Staff = models["Staff"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        with monorm.scope(shop=1, user="ann"):
            row = await Staff.objects.using(db).insert(name="Ada", text="t", start=1, end=2, first="grace", last="hopper")
            assert row.initials == "GH"
            updated = await Staff.objects.using(db).filter(Staff.id == row.id).update(first="mary", last="shelley").returning()
            assert [r.initials for r in updated] == ["MS"]
    finally:
        await db.close()


RETURNED = """
model Person {
 id Int @id @default(autoincrement())
 name String
 bio String?
 @@map("composition_returned_people")
}
model Staff {
 salary Int
 badge String?
 @@composition.model(parent: "Person", parentRef: "person", childRef: "staff")
 @@query.defaults(fields: ["name", "salary"])
 @@map("composition_returned_staff")
}
"""


@pytest.mark.skipif(not has_extension("monorm.query-defaults"), reason="requires the monorm.query-defaults package")
async def test_a_multi_table_write_returns_the_default_selection_or_the_named_columns(observe_queries):
    registry = monorm.Registry()
    models = monorm.loads('datasource db {\n provider = "sqlite"\n}\n' + RETURNED, registry=registry)
    Staff = models["Staff"]
    db = await monorm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    try:
        staff = Staff.objects.using(db)
        with observe_queries(db) as events:
            row = await staff.insert(name="Ann", salary=1, badge="b", bio="x")
        assert row.to_dict() == {"salary": 1, "name": "Ann"}
        assert '"badge"' not in events[-1].sql and '"bio"' not in events[-1].sql
        with observe_queries(db) as events:
            named = await staff.insert(name="Bob", salary=2).returning(Staff.badge)
        assert named.to_dict() == {"badge": None}
        assert '"badge"' in events[-1].sql and '"salary"' not in events[-1].sql
        everything = await staff.without_defaults().insert(name="Cid", salary=3, bio="z")
        assert (everything.salary, everything.badge, everything.name, everything.bio) == (3, None, "Cid", "z")
        many = await staff.insert_many([{"name": "Dee", "salary": 4}, {"name": "Eve", "salary": 5}]).returning()
        assert [r.to_dict() for r in many] == [{"salary": 4, "name": "Dee"}, {"salary": 5, "name": "Eve"}]
        updated = await staff.filter(Staff.name == "Ann").update(salary=9).returning()
        assert [r.to_dict() for r in updated] == [{"salary": 9, "name": "Ann"}]
        updated = await staff.filter(Staff.name == "Ann").update(salary=10).returning(Staff.badge)
        assert [r.to_dict() for r in updated] == [{"badge": "b"}]
        deleted = await staff.filter(Staff.name == "Dee").delete().returning()
        assert [r.to_dict() for r in deleted] == [{"salary": 4, "name": "Dee"}]
        assert await staff.without_defaults().count() == 4
    finally:
        await db.close()
