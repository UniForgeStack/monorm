"""Field permissions `see=` and `change=`: a hidden field leaves every path, through `admin.test_client`, on PostgreSQL and SQLite."""

import csv
import io
import json
import os

import pytest

import monorm
from artifact_caps import has_extension
from monorm import admin
from monorm_auth import UserMethods, UserQueries, make_password

PG_URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
SCHEMA = """datasource db {
  provider = "%s"
}
import "app:monorm-auth" (prefix: "auth_")
import "app:monorm-admin" (prefix: "admin_")

model FpSupplier {
  id       Int         @id @default(autoincrement())
  name     String      @db.VarChar(40)
  iban     String      @db.VarChar(40)
  logo     Json?
  products FpProduct[]
  @@map("fp_suppliers")
}

model FpProduct {
  id          Int        @id @default(autoincrement())
  supplier_id Int
  name        String     @db.VarChar(40)
  code        String     @db.VarChar(20)
  price       Int
  cost        Int
  supplier    FpSupplier @relation(fields: [supplier_id], references: [id])
  notes       FpNote[]
  @@index([name])
  @@index([code])
  @@map("fp_products")
}

model FpNote {
  id         Int       @id @default(autoincrement())
  product_id Int
  body       String    @db.VarChar(40)
  internal   String    @db.VarChar(40)
  product    FpProduct @relation(fields: [product_id], references: [id], onDelete: Cascade)
  @@map("fp_notes")
}
"""
API = "/admin/api"
PRODUCTS = f"{API}/resources/FpProduct"
BASE = ["admin.access", "admin.manage_fpproduct", "admin.manage_fpnote", "admin.manage_fpsupplier"]
COST = ["admin.see_cost"]
EVERY = ["admin.see_cost", "admin.see_code", "admin.change_price", "admin.see_internal", "admin.see_supplier_name", "admin.see_logo"]


def table(response):
    assert response.status == 200, response.text
    return list(csv.reader(io.StringIO(response.text, newline="")))


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
    using = lambda name: models[name].objects.using(db)  # noqa: E731
    pw = make_password("pw")
    await using("User").insert_many([{"username": name, "email": f"{name}@example.com", "password": pw} for name in ("ann", "bob", "cy", "dee")])
    await using("FpSupplier").insert_many([{"name": "Acme", "iban": "DE01"}, {"name": "Bolt", "iban": "DE02"}])
    await using("FpProduct").insert_many(
        [
            {"supplier_id": 1, "name": "lamp", "code": "L-1", "price": 20, "cost": 7},
            {"supplier_id": 2, "name": "desk", "code": "D-1", "price": 90, "cost": 40},
            {"supplier_id": 1, "name": "chair", "code": "C-1", "price": 50, "cost": 21},
        ]
    )
    await using("FpNote").insert_many([{"product_id": 1, "body": "fragile", "internal": "margin 65%"}])
    yield World(registry, models, db)
    await db.drop_tables()
    await db.close()


class Media:
    """A storage registry stand-in: OpenDAL's memory service holds the bytes."""

    @staticmethod
    def make():
        from monorm_file_storage import Registry
        from opendal import AsyncOperator

        return Registry({"media": AsyncOperator("memory")})


class World:
    def __init__(self, registry, models, db):
        self.registry, self.models, self.db = registry, models, db

    def site(self, save="row", files=False, **options):
        if files:
            options["storage"] = Media.make()
        site = admin.Admin(self.registry, **options)
        p, n, s = self.models["FpProduct"], self.models["FpNote"], self.models["FpSupplier"]
        site.register(
            p,
            {
                "list": {
                    "columns": [p.name, p.code, p.price, p.cost],
                    "order": [p.id],
                    "search": [p.name, p.code],
                    "filters": [p.code, p.cost, p.price],
                    "sortable": [p.name, p.cost, p.price],
                },
                "see": {"cost": "see_cost", "code": "see_code"},
                "change": {"price": "change_price"},
                "relations": {"supplier": {}},
                "inlines": [admin.inline(n, save=save)],
            },
        )
        site.register(n, {"see": {"internal": "see_internal"}})
        site.register(s, {"list": {"columns": [s.name, s.iban], "order": [s.id], "search": [s.name]}, "see": {"name": "see_supplier_name", "logo": "see_logo"}, **({"files": {"logo": "media"}} if files else {})})
        site.register(self.models["Role"])
        site.register(self.models["Permission"], {"allow": ["read"]})
        return site

    async def users(self, site):
        """ann holds the page names only; bob holds every field name; cy holds `see_cost` only; dee holds `see_supplier_name` only."""
        await site.sync_permissions(self.db)
        given = {"ann": BASE, "bob": BASE + EVERY, "cy": BASE + COST, "dee": BASE + ["admin.see_supplier_name"]}
        clients = {}
        for username, names in given.items():
            clients[username] = site.test_client(self.db, user={"key": await grant(self.db, self.models, username, names)})
        return clients


async def grant(db, models, username, names):
    perms = models["Permission"]
    rows = [r for r in await perms.objects.using(db) if f"{r.label}.{r.codename}" in names]
    assert len(rows) == len(names), names
    Role = models["Role"]
    await Role.objects.using(db).insert_many([{"name": f"for-{username}"}])
    role = await Role.objects.using(db).filter(Role.name == f"for-{username}").first()
    user = await models["User"].objects.using(db).filter(models["User"].username == username).first()
    await models["RolePermission"].objects.using(db).insert_many([{"role_id": role.id, "permission_id": p.id} for p in rows])
    await models["UserRole"].objects.using(db).insert_many([{"user_id": user.id, "role_id": role.id}])
    return user.id


def labels(columns):
    return [c["label"] for c in columns]


def names_of(rows, index=0):
    return [r[index] for r in rows]


async def test_a_name_binds_a_field_and_an_unbound_field_follows_the_page(world):
    site = world.site()
    c = await world.users(site)
    assert {"admin.see_cost", "admin.see_code", "admin.change_price", "admin.see_internal", "admin.see_supplier_name"} <= set(site.permissions)
    ann, cy, bob = [(await c[u].get(PRODUCTS)).json() for u in ("ann", "cy", "bob")]
    assert labels(ann["columns"])[:3] == ["name", "price", "id"]
    assert labels(cy["columns"])[:3] == ["name", "price", "cost"]
    assert labels(bob["columns"])[:4] == ["name", "code", "price", "cost"]
    assert ann["rows"][0][:2] == ["lamp", 20] and len(ann["rows"][0]) == 3
    assert [r[2] for r in cy["rows"]] == [7, 40, 21]
    assert bob["rows"][1][:4] == ["desk", "D-1", 90, 40]


async def test_the_metadata_leaves_out_a_field_the_user_cannot_see(world):
    c = await world.users(world.site())
    meta = {u: {r["name"]: r for r in (await c[u].get(API + "/resources")).json()["resources"]} for u in ("ann", "bob")}
    ann, bob = meta["ann"]["FpProduct"], meta["bob"]["FpProduct"]
    for part in ("columns", "fields", "form"):
        assert not {"cost", "code"} & {x.get("label") or x.get("name") for x in ann[part]}, part
        assert {"cost", "code"} <= {x.get("label") or x.get("name") for x in bob[part]}, part
    assert [f["name"] for f in ann["filters"]] == ["price"] and {"code", "cost", "price"} == {f["name"] for f in bob["filters"]}
    assert "internal" not in str(meta["ann"]["FpNote"]) and "internal" in str(meta["bob"]["FpNote"])
    assert "name" in str(meta["bob"]["FpSupplier"]) and "iban" in str(meta["ann"]["FpSupplier"])
    assert next(f for f in ann["form"] if f["name"] == "price")["readonly"] is True
    assert next(f for f in bob["form"] if f["name"] == "price")["readonly"] is False


async def test_the_detail_page_leaves_out_a_hidden_field(world):
    c = await world.users(world.site())
    ann, cy, bob = [(await c[u].get(f"{PRODUCTS}/1")).json() for u in ("ann", "cy", "bob")]
    assert labels(ann["fields"]) == ["id", "supplier_id", "name", "price"] and ann["values"] == [1, 1, "lamp", 20]
    assert cy["values"] == [1, 1, "lamp", 20, 7]
    assert bob["values"] == [1, 1, "lamp", "L-1", 20, 7]
    assert "L-1" not in str(ann) and "L-1" not in str(cy)


async def test_search_matches_only_a_field_the_user_can_see(world):
    c = await world.users(world.site())
    assert names_of((await c["bob"].get(f"{PRODUCTS}?q=L-1")).json()["rows"]) == ["lamp"]
    assert (await c["ann"].get(f"{PRODUCTS}?q=L-1")).json()["rows"] == []
    assert names_of((await c["ann"].get(f"{PRODUCTS}?q=lamp")).json()["rows"]) == ["lamp"]


async def test_a_filter_or_a_sort_on_a_hidden_field_is_an_error(world):
    c = await world.users(world.site())
    ann, cy = c["ann"], c["cy"]
    assert names_of((await ann.get(f"{PRODUCTS}?f.price=50")).json()["rows"]) == ["chair"]
    for hidden in (f"{PRODUCTS}?f.cost=7", f"{PRODUCTS}?f.code=L-1", f"{PRODUCTS}?sort=cost", f"{API}/counts/FpProduct?f.cost=7", f"{API}/export/FpProduct?f.cost=7"):
        reply = await ann.get(hidden)
        assert reply.status == 400, hidden
    assert (await ann.get(f"{PRODUCTS}?sort=-price")).status == 200
    assert names_of((await cy.get(f"{PRODUCTS}?f.cost=7")).json()["rows"]) == ["lamp"]
    assert names_of((await cy.get(f"{PRODUCTS}?sort=-cost")).json()["rows"]) == ["desk", "chair", "lamp"]
    assert (await cy.get(f"{PRODUCTS}?f.code=L-1")).status == 400


async def test_the_export_has_no_hidden_column(world):
    c = await world.users(world.site())
    export = f"{API}/export/FpProduct"
    assert table(await c["ann"].get(export))[0] == ["Name", "Price", "Id"]
    assert table(await c["cy"].get(export))[0][:3] == ["Name", "Price", "Cost"]
    assert table(await c["bob"].get(export))[0][:4] == ["Name", "Code", "Price", "Cost"]
    assert "L-1" not in (await c["ann"].get(export)).text and "L-1" in (await c["bob"].get(export)).text


async def test_a_picker_label_that_the_user_cannot_see_falls_back_to_the_key(world):
    c = await world.users(world.site())
    url = f"{API}/autocomplete/FpProduct/supplier"
    assert [i["label"] for i in (await c["bob"].get(url)).json()["items"]] == ["Acme", "Bolt"]
    ann = await c["ann"].get(url)
    assert [i["label"] for i in ann.json()["items"]] == [1, 2] and "Acme" not in ann.text
    assert (await c["ann"].get(f"{url}?q=Acme")).status == 400, "the label column is the only search field and ann cannot see it"


async def test_a_tab_and_an_inline_leave_out_a_hidden_field(world):
    c = await world.users(world.site())
    tab = f"{PRODUCTS}/1/notes"
    ann, bob = (await c["ann"].get(tab)).json(), (await c["bob"].get(tab)).json()
    assert labels(ann["columns"]) == ["id", "product_id", "body"] and ann["rows"] == [[1, 1, "fragile"]]
    assert labels(bob["columns"])[-1] == "internal" and bob["rows"] == [[1, 1, "fragile", "margin 65%"]]
    inline = lambda reply: reply.json()["inlines"]["notes"]["rows"][0]["values"]  # noqa: E731
    assert inline(await c["ann"].get(f"{PRODUCTS}/1")) == [1, 1, "fragile"]
    assert inline(await c["bob"].get(f"{PRODUCTS}/1")) == [1, 1, "fragile", "margin 65%"]
    assert (await c["ann"].get(f"{API}/counts/FpProduct/1/notes")).json()["count"] == 1


async def test_a_write_on_a_field_the_user_cannot_see_or_change_is_a_422(world):
    c = await world.users(world.site())
    ann, bob = c["ann"], c["bob"]
    product = world.models["FpProduct"]
    created = await ann.post(PRODUCTS, json={"set": {"supplier_id": 1, "name": "rug", "code": "R-1", "price": 5, "cost": 1}})
    assert created.status == 422, created.text
    assert set(created.json()["fields"]) == {"code", "cost", "price"}
    locked = await ann.patch(f"{PRODUCTS}/1", json={"set": {"price": 99}, "loaded": {"price": 20}})
    assert (locked.status, locked.json()["fields"]["price"]) == (422, ["This field is read-only."]), locked.text
    unseen = await ann.patch(f"{PRODUCTS}/1", json={"set": {"cost": 1}, "loaded": {"cost": 7}})
    assert unseen.status == 422 and "cost" in unseen.json()["fields"], unseen.text
    renamed = await ann.patch(f"{PRODUCTS}/1", json={"set": {"name": "lamp2"}, "loaded": {"name": "lamp"}})
    assert renamed.status == 200, renamed.text
    allowed = await bob.patch(f"{PRODUCTS}/1", json={"set": {"price": 25, "cost": 8}, "loaded": {"price": 20, "cost": 7}})
    assert allowed.status == 200, allowed.text
    row = await product.objects.using(world.db).filter(product.id == 1).first()
    assert (row.name, row.price, row.cost) == ("lamp2", 25, 8)


async def test_the_response_of_a_write_leaves_out_a_hidden_value(world):
    c = await world.users(world.site())
    changed = await c["ann"].patch(f"{PRODUCTS}/2", json={"set": {"name": "table"}, "loaded": {"name": "desk"}})
    assert changed.status == 200, changed.text
    assert "D-1" not in changed.text and "40" not in str(changed.json().get("values", []))
    made = await c["bob"].post(PRODUCTS, json={"set": {"supplier_id": 1, "name": "rug", "code": "R-1", "price": 5, "cost": 1}})
    assert made.status in (200, 201) and "R-1" in made.text, made.text


async def test_a_resource_without_a_bound_name_serves_the_page_to_everyone(world):
    c = await world.users(world.site())
    supplier = (await c["ann"].get(f"{API}/resources/FpSupplier")).json()
    assert labels(supplier["columns"])[:1] == ["iban"] and "Acme" not in str(supplier)


async def test_a_bad_binding_is_an_admin_config_error(world):
    p, n = world.models["FpProduct"], world.models["FpNote"]
    bad = [
        ({"see": {"nope": "see_nope"}}, "nope"),
        ({"see": {"id": "see_id"}}, "key"),
        ({"change": {"nope": "change_nope"}}, "nope"),
        ({"see": {"cost": "Bad Name"}}, "lowercase"),
        ({"see": {"cost": 3}}, "see"),
    ]
    for config, text in bad:
        candidate = admin.Admin(world.registry)
        candidate.register(p, config)  # type: ignore[arg-type]
        with pytest.raises(admin.AdminConfigError, match=text):
            candidate.check()
    inline = admin.Admin(world.registry)
    inline.register(p, {"inlines": [admin.inline(n, page={"see": {"internal": "see_internal"}})]})  # type: ignore[typeddict-unknown-key]
    with pytest.raises(admin.AdminConfigError, match="registered page"):
        inline.check()
    strategy = admin.Admin(world.registry, permissions="access")
    strategy.register(p, {"see": {"cost": "see_cost"}})
    with pytest.raises(admin.AdminConfigError, match="need the page or model"):
        strategy.check()



async def test_every_other_route_leaks_no_hidden_value_to_a_user_without_the_name(world):
    c = await world.users(world.site())
    ann = c["ann"]
    secrets = ("margin 65%", "L-1", "D-1", "C-1", "DE01", "DE02")
    for uri in (
        f"{PRODUCTS}", f"{PRODUCTS}/1", f"{PRODUCTS}/1/notes", f"{PRODUCTS}?q=L-1", f"{API}/export/FpProduct", f"{API}/counts/FpProduct", f"{API}/counts/FpProduct/1/notes",
        f"{API}/links/FpProduct/1/supplier", f"{API}/autocomplete/FpProduct/supplier", f"{API}/forms/FpProduct", f"{API}/resources",
    ):
        reply = await ann.get(uri)
        assert not any(secret in reply.text for secret in secrets), uri
    for hidden in (f"{PRODUCTS}/1/notes?f.internal=x", f"{PRODUCTS}/1/notes?sort=internal", f"{API}/counts/FpProduct/1/notes?f.internal=x", f"{API}/export/FpProduct?sort=cost"):
        assert (await ann.get(hidden)).status == 400, hidden
    stale = await ann.patch(f"{PRODUCTS}/1", json={"set": {"name": "x"}, "loaded": {"name": "stale"}})
    assert stale.status == 409 and not any(secret in stale.text for secret in secrets) and "cost" not in stale.text, stale.text
    note = await ann.post(f"{PRODUCTS}/1/inlines/notes", json={"set": {"body": "b", "internal": "x"}})
    assert note.status == 422, note.text


async def test_a_view_belongs_to_a_granted_set_not_to_the_first_user_of_it(world):
    await world.users(world.site())
    for order in (("ann", "dee"), ("dee", "ann")):
        site = world.site()
        keys = {"ann": 1, "dee": 4}
        seen = {}
        for name in order:
            client = site.test_client(world.db, user={"key": keys[name]})
            seen[name] = [i["label"] for i in (await client.get(f"{API}/autocomplete/FpProduct/supplier")).json()["items"]]
            assert labels((await client.get(PRODUCTS)).json()["columns"]) == ["name", "price", "id"]
        assert seen == {"ann": [1, 2], "dee": ["Acme", "Bolt"]}, order


async def test_a_create_that_a_hidden_required_field_blocks_names_the_field(world):
    c = await world.users(world.site())
    blocked = await c["ann"].post(PRODUCTS, json={"set": {"supplier_id": 1, "name": "rug"}})
    assert blocked.status == 422, blocked.text
    assert blocked.json()["fields"] and set(blocked.json()["fields"]) <= {"code", "cost", "price"}, blocked.text
    assert await world.models["FpProduct"].objects.using(world.db).count() == 3


async def test_a_bulk_delete_whose_filter_names_a_hidden_field_is_a_400_and_deletes_nothing(world):
    c = await world.users(world.site())
    delete = f"{API}/actions/FpProduct/bulk_delete"
    hidden = await c["ann"].post(f"{delete}?f.cost=7", json={"all": True, "inputs": {}})
    assert hidden.status == 400, hidden.text
    assert await world.models["FpProduct"].objects.using(world.db).count() == 3
    shown = await c["cy"].post(f"{delete}?f.cost=7", json={"all": True, "inputs": {}})
    assert shown.status == 200, shown.text
    product = world.models["FpProduct"]
    assert sorted(r.name for r in await product.objects.using(world.db)) == ["chair", "desk"]
    visible = await c["ann"].post(f"{delete}?f.price=50", json={"all": True, "inputs": {}})
    assert visible.status == 200, visible.text
    assert [r.name for r in await product.objects.using(world.db)] == ["desk"]


async def test_an_inline_written_with_its_parent_leaves_out_a_hidden_field(world):
    c = await world.users(world.site(save="with_parent"))
    notes = world.models["FpNote"]
    edit = {"set": {"name": "lamp2"}, "loaded": {"name": "lamp"}}
    hidden = await c["ann"].patch(f"{PRODUCTS}/1", json={**edit, "inlines": {"notes": {"create": [{"set": {"body": "b", "internal": "x"}}]}}})
    assert hidden.status == 422, hidden.text
    assert "x" not in str(hidden.json().get("fields", {})) and await notes.objects.using(world.db).count() == 1
    product = world.models["FpProduct"]
    assert (await product.objects.using(world.db).filter(product.id == 1).first()).name == "lamp", "the failed change set is not stored"
    allowed = await c["bob"].patch(f"{PRODUCTS}/1", json={**edit, "inlines": {"notes": {"create": [{"set": {"body": "b", "internal": "x"}}]}}})
    assert allowed.status == 200, allowed.text
    assert sorted(r.internal for r in await notes.objects.using(world.db)) == ["margin 65%", "x"]


@pytest.mark.skipif(not has_extension("monorm.file-storage"), reason="requires file-storage")
async def test_a_file_field_that_the_user_cannot_see_takes_no_upload_and_gives_no_download(world):
    c = await world.users(world.site(files=True))
    path = f"{API}/files/FpSupplier/1/logo"
    up = await c["bob"].post(path, file=("logo.png", b"png-bytes", "image/png"))
    assert up.status == 200, up.text
    assert (await c["bob"].get(path)).content == b"png-bytes"
    assert (await c["ann"].get(path)).status == 404
    assert (await c["ann"].post(path, file=("evil.png", b"x", "image/png"))).status == 404
    assert (await c["bob"].get(path)).content == b"png-bytes", "the hidden upload changed nothing"


async def test_the_audit_log_drops_the_values_of_a_field_the_reader_cannot_see(world):
    site = world.site()
    await site.sync_permissions(world.db)
    readers = {}
    for username, extra in (("ann", []), ("cy", COST)):
        key = await grant(world.db, world.models, username, BASE + extra + ["admin.read_adminlogentry"])
        readers[username] = site.test_client(world.db, user={"key": key})
    bob = site.test_client(world.db, user={"key": await grant(world.db, world.models, "bob", BASE + EVERY)})
    edited = await bob.patch(f"{PRODUCTS}/1", json={"set": {"name": "lamp2", "cost": 8}, "loaded": {"name": "lamp", "cost": 7}})
    assert edited.status == 200, edited.text

    async def diff(client):
        reply = await client.get(f"{API}/resources/AdminLogEntry")
        assert reply.status == 200, reply.text
        body = reply.json()
        column = labels(body["columns"]).index("changes")
        return json.loads(body["rows"][0][column]) if isinstance(body["rows"][0][column], str) else body["rows"][0][column]

    ann = await diff(readers["ann"])
    assert ann["diff"] == {"name": {"from": "lamp", "to": "lamp2"}}
    cy = await diff(readers["cy"])
    assert cy["diff"]["cost"] == {"from": 7, "to": 8}
    for username, sees in (("ann", False), ("cy", True)):
        detail = await readers[username].get(f"{API}/resources/AdminLogEntry/1")
        assert detail.status == 200, detail.text
        assert ("cost" in json.dumps(detail.json())) is sees
        export = await readers[username].get(f"{API}/export/AdminLogEntry")
        assert export.status == 200, export.text
        assert ("cost" in export.text) is sees
