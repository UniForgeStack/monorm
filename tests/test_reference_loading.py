"""Explicit instance loads run on the instance context and cache per owner."""
import asyncio

import pytest
import orm
from conftest import DATABASE_URL
from orm.query import QuerySet

SOURCE = '''
model Owner {
  id Int @id
  name String
  required Required[]
  optional Optional[]
  detail Detail?
  role Role?
  @@map("ref04_owners")
}
model Required {
  id Int @id
  owner_id Int
  owner Owner @relation(fields: [owner_id], references: [id])
  @@map("ref04_required")
}
model Optional {
  id Int @id
  owner_id Int?
  owner Owner? @relation(fields: [owner_id], references: [id])
  @@map("ref04_optional")
}
model Detail {
  id Int @id
  owner_id Int @unique
  owner Owner @relation(fields: [owner_id], references: [id])
  @@map("ref04_details")
}
model Role {
  id Int @id
  person Owner @relation(fields: [id], references: [id])
  @@map("ref04_roles")
}
'''


async def load_relation(row, name, *, reload=False):
    assert await row.load(getattr(type(row), name), reload=reload) is row
    return getattr(row, name)


@pytest.fixture(params=["sqlite", "postgres"])
async def references(request):
    dialect = request.param
    registry = orm.Registry()
    models = orm.loads(f'datasource db {{\n provider = "{dialect}"\n}}\n' + SOURCE, registry=registry)
    url = "sqlite://:memory:" if dialect == "sqlite" else DATABASE_URL
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        yield db, models
    finally:
        await db.drop_tables()
        await db.close()


async def seed(db, m):
    await m["Owner"].objects.using(db).insert(id=1, name="one")
    await m["Owner"].objects.using(db).insert(id=2, name="two")
    return await m["Required"].objects.using(db).insert(id=1, owner_id=1)


async def test_identity_reload_fk_invalidation_and_counts(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    calls = 0
    original = QuerySet.first

    async def first(qs):
        nonlocal calls
        calls += 1
        return await original(qs)

    monkeypatch.setattr(QuerySet, "first", first)
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    results = await asyncio.gather(*(load_relation(row, "owner") for _ in range(12)))
    assert calls == 1
    assert all(value is results[0] for value in results)
    assert row.owner is results[0]
    assert await load_relation(row, "owner") is row.owner
    assert calls == 1
    await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).update(name="changed")
    fresh = await load_relation(row, "owner", reload=True)
    assert fresh is row.owner and fresh is not results[0] and fresh.name == "changed"
    assert calls == 2
    await row.update(owner_id=2)
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    assert (await load_relation(row, "owner")).id == 2
    assert calls == 3
    await m["Required"].objects.using(db).update(owner_id=1)
    await row.refresh()
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    assert (await load_relation(row, "owner")).id == 1
    eager = await m["Required"].objects.using(db).load(m["Required"].owner).get()
    assert await load_relation(eager, "owner") is eager.owner
    assert calls == 4


async def test_cached_absence_refresh_reverse_and_null(references, monkeypatch):
    db, m = references
    await seed(db, m)
    owner = await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).get()
    calls = 0
    original = QuerySet.first

    async def first(qs):
        nonlocal calls
        calls += 1
        return await original(qs)

    monkeypatch.setattr(QuerySet, "first", first)
    assert await load_relation(owner, "detail") is None
    assert owner.detail is None
    assert await load_relation(owner, "detail") is None and calls == 1
    await m["Detail"].objects.using(db).insert(id=1, owner_id=1)
    assert await load_relation(owner, "detail") is None
    await owner.refresh()
    with pytest.raises(orm.NotLoaded):
        _ = owner.detail
    assert (await load_relation(owner, "detail")).id == 1 and calls == 2
    nullable = await m["Optional"].objects.using(db).insert(id=1, owner_id=None)
    assert await load_relation(nullable, "owner") is None and calls == 2
    assert await load_relation(nullable, "owner", reload=True) is None and calls == 2


async def test_required_integrity_optional_missing_and_retry(references):
    db, m = references
    row = await seed(db, m)
    nullable = await m["Optional"].objects.using(db).insert(id=1, owner_id=1)
    # Emulate physically corrupt legacy data without changing server constraints.
    row.__dict__["owner_id"] = 999
    nullable.__dict__["owner_id"] = 999
    with pytest.raises(orm.IntegrityError, match="required target"):
        await load_relation(row, "owner")
    assert not row.__dict__["_reference_pending"]
    assert await load_relation(nullable, "owner") is None
    assert nullable.owner is None
    assert await load_relation(nullable, "owner") is None
    row.__dict__["owner_id"] = 1
    assert (await load_relation(row, "owner")).id == 1


async def test_dangling_key_message_says_why(references):
    db, m = references
    row = await seed(db, m)
    with pytest.raises(orm.NotLoaded) as plain:
        _ = row.owner
    assert "no row of the target" not in str(plain.value)
    # Emulate corrupt data: the join found no target for a required key.
    row.__dict__["owner"] = None
    with pytest.raises(orm.NotLoaded, match=r"is not loaded: the key is set and no row of the target has it; use await obj\.load\(Required\.owner\)"):
        _ = row.owner


async def test_transaction_and_separate_database_context(references):
    db, m = references
    await seed(db, m)
    row = await m["Required"].objects.using(db).get()
    with pytest.raises(RuntimeError, match="rollback"):
        async with db.transaction():
            await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).update(name="uncommitted")
            assert (await load_relation(row, "owner")).name == "uncommitted"
            raise RuntimeError("rollback")
    assert (await load_relation(row, "owner", reload=True)).name == "one"


async def test_cancelled_waiter_failure_and_inflight_refresh(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    gate = asyncio.Event()
    started = asyncio.Event()
    original = QuerySet.first
    calls = 0

    async def slow(qs):
        nonlocal calls
        calls += 1
        value = await original(qs)
        started.set()
        await gate.wait()
        return value

    monkeypatch.setattr(QuerySet, "first", slow)
    one = asyncio.create_task(row.load(m["Required"].owner))
    await started.wait()
    two = asyncio.create_task(row.load(m["Required"].owner))
    await asyncio.sleep(0)
    one.cancel()
    with pytest.raises(asyncio.CancelledError):
        await one
    await row.update(owner_id=2)
    gate.set()
    assert await two is row
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    assert (await load_relation(row, "owner")).id == 2
    assert calls == 2

    async def fail(qs):
        raise RuntimeError("temporary failure")

    monkeypatch.setattr(QuerySet, "first", fail)
    with pytest.raises(RuntimeError, match="temporary"):
        await load_relation(row, "owner", reload=True)
    monkeypatch.setattr(QuerySet, "first", original)
    assert (await load_relation(row, "owner", reload=True)).id == 2


def test_collision_rejected_atomically():
    registry = orm.Registry()
    before = registry.ir()
    with pytest.raises(TypeError, match="reserved"):
        orm.loads(SOURCE.replace("owner_id Int\n", "owner_id Int\n  load String\n", 1), registry=registry)
    assert registry.ir() == before


async def test_hidden_dependency_keys_and_missing_key_fail_before_io(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    row.__dict__["_orm_internal"] = {"owner_id": row.__dict__.pop("owner_id")}
    owner = await load_relation(row, "owner")
    assert row.owner is owner
    owner.__dict__["_orm_internal"] = {"id": owner.__dict__.pop("id")}
    assert await load_relation(row, "owner") is owner
    assert row.owner is owner
    assert owner.id == 1
    row.__dict__["_orm_internal"].clear()
    async def forbidden(qs):
        raise AssertionError("must fail before I/O")
    monkeypatch.setattr(QuerySet, "first", forbidden)
    with pytest.raises(orm.NotLoaded, match="owner_id"):
        await load_relation(row, "owner", reload=True)


async def test_update_with_an_unloaded_reference_key(references):
    db, m = references
    row = await seed(db, m)
    row.__dict__.pop("owner_id")
    await row.update(owner_id=2)
    assert (await load_relation(row, "owner")).id == 2


async def test_different_transaction_contexts_do_not_coalesce(references, monkeypatch):
    db, m = references
    row = await seed(db, m)
    gate, started = asyncio.Event(), asyncio.Event()
    original = QuerySet.first
    calls = 0
    async def delayed(qs):
        nonlocal calls
        calls += 1
        value = await original(qs)
        if calls == 1:
            started.set()
            await gate.wait()
        return value
    monkeypatch.setattr(QuerySet, "first", delayed)
    outside = asyncio.create_task(row.load(m["Required"].owner))
    await started.wait()
    try:
        async with db.transaction():
            await m["Owner"].objects.using(db).filter(m["Owner"].id == 1).update(name="inside")
            inside = await load_relation(row, "owner", reload=True)
            assert inside.name == "inside" and calls == 2
            gate.set()
            assert await outside is row
            assert row.owner is inside
    finally:
        gate.set()
        await outside


async def test_named_shared_primary_key_references(references):
    db, m = references
    await seed(db, m)
    role = await m["Role"].objects.using(db).insert(id=1)
    person = await load_relation(role, "person")
    assert person.pk == role.pk and role.person is person
    reverse = await load_relation(person, "role")
    assert reverse.pk == person.pk and person.role is reverse
    assert await load_relation(reverse, "person") is reverse.person


async def test_instance_load_is_additive_and_skips_loaded_fields(references, observe_queries):
    db, m = references
    await seed(db, m)
    Owner = m["Owner"]
    owner = await Owner.objects.using(db).filter(Owner.id == 1).load(Owner.id).get()
    with pytest.raises(orm.NotLoaded):
        _ = owner.name
    with observe_queries(db) as queries:
        assert await owner.load(Owner.name) is owner
        assert owner.id == 1 and owner.name == "one"
        await owner.load(Owner.name)
        await owner.load()
    assert len(queries) == 1
    await Owner.objects.using(db).filter(Owner.id == 1).update(name="changed")
    await owner.load(Owner.name)
    assert owner.name == "one"
    await owner.load(Owner.name, reload=True)
    assert owner.name == "changed" and owner.id == 1
    assert not hasattr(owner, "load_detail")


async def test_instance_load_nested_partial_and_collection_queries(references, observe_queries):
    db, m = references
    row = await seed(db, m)
    Required, Owner = m["Required"], m["Owner"]
    await row.load(Required.owner.name)
    owner = row.owner
    assert owner.name == "one"
    with pytest.raises(orm.NotLoaded):
        _ = owner.id
    await row.load(Required.owner)
    assert row.owner is owner and owner.id == 1
    await owner.load(Owner.required.owner.name)
    assert owner.required.cached[0].owner.name == "one"
    with observe_queries(db) as queries:
        await owner.load(Owner.required.owner.name)
    assert not queries
    filtered = Owner.required.objects.filter(Required.id == 1).load(Required.id).label("selected")
    await owner.load(filtered)
    assert [r.id for r in owner.selected] == [1]
    with pytest.raises(orm.NotLoaded):
        _ = owner.selected[0].owner_id
    await owner.load(Owner.optional)
    assert owner.optional.cached == []
    with observe_queries(db) as queries:
        await owner.load(Owner.optional)
    assert not queries


async def test_instance_load_validates_before_io_and_reload_invalidates_reference(references, observe_queries):
    db, m = references
    row = await seed(db, m)
    Required, Owner = m["Required"], m["Owner"]
    with observe_queries(db) as queries:
        with pytest.raises(TypeError):
            await row.load(Required.owner, Owner.name)
    assert not queries
    await row.load(Required.owner)
    await Required.objects.using(db).update(owner_id=2)
    await row.load(Required.owner_id, reload=True)
    with pytest.raises(orm.NotLoaded):
        _ = row.owner
    await row.load(Required.owner)
    assert row.owner.id == 2


async def test_loading_a_key_and_its_reference_uses_the_new_key(references):
    db, m = references
    await seed(db, m)
    Required = m["Required"]
    row = await Required.objects.using(db).load(Required.id).get()
    await Required.objects.using(db).update(owner_id=2)
    await row.load(Required.owner, Required.owner_id)
    assert row.owner_id == row.owner.id == 2


async def test_nested_load_preserves_cached_collection_membership(references, observe_queries):
    db, m = references
    row = await seed(db, m)
    Required, Owner = m["Required"], m["Owner"]
    owner = await Owner.objects.using(db).filter(Owner.id == 1).get()
    await owner.load(Owner.required.objects.load(Required.id))
    [cached] = owner.required.cached
    await Required.objects.using(db).insert(id=2, owner_id=1)
    await owner.load(Owner.required.owner.name)
    assert owner.required.cached == [row]
    assert owner.required.cached[0] is cached
    assert cached.owner.name == "one"
    partial = Owner.required.objects.load(Required.id).label("selected")
    await owner.load(partial)
    with observe_queries(db) as queries:
        await owner.load(partial)
    assert not queries
    await owner.load(Owner.required, reload=True)
    assert [r.id for r in owner.required.cached] == [1, 2]


async def test_nested_relation_queryset_labels_are_cached(references, observe_queries):
    db, m = references
    await seed(db, m)
    Owner = m["Owner"]
    owner = await Owner.objects.using(db).filter(Owner.id == 1).get()
    nested = Owner.required.owner.objects.load(Owner.name).label("person")
    await owner.load(nested)
    assert owner.required.cached[0].person.name == "one"
    with observe_queries(db) as queries:
        await owner.load(nested)
    assert not queries
