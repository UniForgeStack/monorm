import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, prefetch, Decimal, Registry, IntegrityError, NotLoaded, type Database } from "../src/index.js";
import { INTERNAL } from "../src/model.js";
import { QuerySet } from "../src/query.js";
import { DATABASE_URL } from "./helpers.js";

const source = `
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
}`;
// Runtime schemas intentionally have no static model types. Generated APIs are checked in Rust/type fixtures.
type Dynamic = Record<string, any>;
async function loadRelation(row: Dynamic, name: string, reload = false): Promise<any> {
  assert.equal(await row.load((row["constructor"] as any).meta.model[name], { reload }), row);
  return row[name];
}
for (const dialect of ["sqlite", "postgres"]) {
  test(`${dialect}: explicit loads, identity, reload, absence, integrity, invalidation, context and races`, async () => {
    const registry = new Registry();
    const m = loads(`datasource db {\n provider = "${dialect}"\n}\n${source}`, { registry }) as Dynamic;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : DATABASE_URL;
    const db = await connect(url, { registry, default: false });
    const original = QuerySet.prototype.first;
    let calls = 0;
    await db.dropTables();
    await db.createTables();
    try {
      const Owner = m["Owner"], Required = m["Required"], Optional = m["Optional"], Detail = m["Detail"];
      await Owner.objects.using(db).insert({ id: 1, name: "one" });
      await Owner.objects.using(db).insert({ id: 2, name: "two" });
      const row = await Required.objects.using(db).insert({ id: 1, ownerId: 1 });
      QuerySet.prototype.first = async function (...args) { calls++; return original.apply(this, args); };
      assert.throws(() => row.owner, NotLoaded);
      const values = await Promise.all(Array.from({ length: 12 }, () => loadRelation(row, "owner")));
      assert.equal(calls, 1);
      assert.ok(values.every((value) => value === values[0]));
      assert.equal(row.owner, values[0]);
      assert.equal(await loadRelation(row, "owner"), row.owner);
      await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "changed" });
      assert.notEqual(await loadRelation(row, "owner", true), values[0]);
      assert.equal(row.owner.name, "changed");
      await row.update({ ownerId: 2 });
      assert.throws(() => row.owner, NotLoaded);
      assert.equal((await loadRelation(row, "owner")).id, 2);
      await Required.objects.using(db).update({ ownerId: 1 });
      await row.refresh();
      assert.throws(() => row.owner, NotLoaded);
      assert.equal((await loadRelation(row, "owner")).id, 1);
      const eager = await Required.objects.using(db).load(Required.owner).get();
      assert.equal(await loadRelation(eager, "owner"), eager.owner);
      assert.equal(calls, 4);
      const owner = await Owner.objects.using(db).filter(Owner.id.eq(1)).get();
      assert.equal(await loadRelation(owner, "detail"), null);
      assert.equal(await loadRelation(owner, "detail"), null);
      assert.equal(calls, 5);
      await Detail.objects.using(db).insert({ id: 1, ownerId: 1 });
      assert.equal(await loadRelation(owner, "detail"), null);
      await owner.refresh();
      assert.throws(() => owner.detail, NotLoaded);
      assert.equal((await loadRelation(owner, "detail")).id, 1);
      const optional = await Optional.objects.using(db).insert({ id: 1, ownerId: null });
      assert.equal(await loadRelation(optional, "owner"), null);
      assert.equal(await loadRelation(optional, "owner", true), null);
      assert.equal(calls, 6);
      optional.ownerId = 999;
      assert.equal(await loadRelation(optional, "owner"), null);
      assert.equal(optional.owner, null);
      assert.equal(await loadRelation(optional, "owner"), null);
      row.ownerId = 999;
      await assert.rejects(loadRelation(row, "owner", true), IntegrityError);
      row.ownerId = 1;
      assert.equal((await loadRelation(row, "owner")).id, 1);
      await assert.rejects(db.transaction(async () => {
        await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "uncommitted" });
        assert.equal((await loadRelation(row, "owner", true)).name, "uncommitted");
        throw new Error("rollback");
      }), /rollback/);
      assert.equal((await loadRelation(row, "owner", true)).name, "changed");

      let releaseOutside!: () => void;
      let outsideStarted!: () => void;
      const outsideGate = new Promise<void>((done) => { releaseOutside = done; });
      const outsideReady = new Promise<void>((done) => { outsideStarted = done; });
      let contextQueries = 0;
      QuerySet.prototype.first = async function (...args) {
        contextQueries++;
        const value = await original.apply(this, args);
        if (contextQueries === 1) { outsideStarted(); await outsideGate; }
        return value;
      };
      const outsideLoad = row.load(Required.owner, { reload: true });
      await outsideReady;
      await db.transaction(async () => {
        await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "context-specific" });
        const inside = await loadRelation(row, "owner", true);
        assert.equal(inside.name, "context-specific");
        assert.equal(contextQueries, 2);
        releaseOutside();
        assert.equal(await outsideLoad, row);
        assert.equal(row.owner, inside);
      });

      let release!: () => void;
      let start!: () => void;
      const gate = new Promise<void>((done) => { release = done; });
      const started = new Promise<void>((done) => { start = done; });
      QuerySet.prototype.first = async function (...args) {
        const value = await original.apply(this, args);
        start(); await gate;
        return value;
      };
      const inFlight = row.load(Required.owner, { reload: true });
      await started;
      await row.update({ ownerId: 2 });
      release();
      assert.equal(await inFlight, row);
      assert.throws(() => row.owner, NotLoaded);
      assert.equal((await loadRelation(row, "owner")).id, 2);
      QuerySet.prototype.first = async function () { throw new Error("temporary"); };
      await assert.rejects(loadRelation(row, "owner", true), /temporary/);
      QuerySet.prototype.first = original;
      assert.equal((await loadRelation(row, "owner", true)).id, 2);
      // Helper-only source keys remain sufficient for loading without becoming public.
      const hidden = await Required.objects.using(db).get();
      hidden[INTERNAL] = { ownerId: hidden.ownerId };
      delete hidden.ownerId;
      assert.equal((await loadRelation(hidden, "owner")).id, 2);
      assert.equal(hidden.owner.id, 2);
      delete hidden[INTERNAL];
      await hidden.update({ ownerId: 1 });
      assert.equal((await loadRelation(hidden, "owner")).id, 1);
      const role = await m["Role"].objects.using(db).insert({ id: 1 });
      const person = await loadRelation(role, "person");
      assert.equal(role.person, person);
      assert.equal(person.pk, role.pk);
      const reverse = await loadRelation(person, "role");
      assert.equal(person.role, reverse);
      assert.equal(reverse.pk, person.pk);
    } finally {
      QuerySet.prototype.first = original;
      await db.dropTables();
      await db.close();
    }
  });
}

test("load member collision fails atomically", () => {
  const registry = new Registry();
  assert.throws(() => loads(source.replace("owner_id Int\n", "owner_id Int\n load String\n"), { registry }), /reserved/);
  assert.equal([...registry].length, 0);
});

for (const dialect of ["sqlite", "postgres"]) {
  test(`${dialect}: instance load fills partial rows and accepts nested paths and relation querysets`, async () => {
    const registry = new Registry();
    const m = loads(`datasource db {\n provider = "${dialect}"\n}\n${source}`, { registry }) as Dynamic;
    const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const Owner = m["Owner"], Required = m["Required"];
      await Owner.objects.using(db).insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]);
      await Required.objects.using(db).insert({ id: 1, ownerId: 1 });
      const partial = await Owner.objects.using(db).filter(Owner.id.eq(1)).load(Owner.id).get();
      assert.throws(() => partial.name, NotLoaded);
      const queries: unknown[] = [];
      const remove = db.onQuery((event) => queries.push(event));
      assert.equal(await partial.load(Owner.name), partial);
      assert.equal(partial.id, 1);
      assert.equal(partial.name, "one");
      await partial.load(Owner.name);
      await partial.load();
      remove();
      assert.equal(queries.length, 1);
      await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "changed" });
      await partial.load(Owner.name);
      assert.equal(partial.name, "one");
      await partial.load(Owner.name, { reload: true });
      assert.equal(partial.name, "changed");
      const row = await Required.objects.using(db).get();
      await row.load(Required.owner.name);
      const owner = row.owner;
      assert.equal(owner.name, "changed");
      assert.throws(() => owner.id, NotLoaded);
      await row.load(Required.owner);
      assert.equal(row.owner, owner);
      assert.equal(owner.id, 1);
      await owner.load(Owner.required.owner.name);
      assert.equal(owner.required.cached[0].owner.name, "changed");
      await owner.load(Owner.required.objects.filter(Required.id.eq(1)).load(Required.id).label("selected"));
      assert.deepEqual(owner.selected.map((r: Dynamic) => r.id), [1]);
      assert.throws(() => owner.selected[0].ownerId, NotLoaded);
      await owner.load(Owner.optional);
      assert.deepEqual(owner.optional.cached, []);
      const snapshot = await Owner.objects.using(db).filter(Owner.id.eq(1)).get();
      await snapshot.load(Owner.required.objects.load(Required.id));
      const cached = snapshot.required.cached[0];
      await Required.objects.using(db).insert({ id: 2, ownerId: 1 });
      await snapshot.load(Owner.required.owner.name);
      assert.deepEqual(snapshot.required.cached.map((r: Dynamic) => r.id), [1]);
      assert.equal(snapshot.required.cached[0], cached);
      assert.equal(cached.owner.name, "changed");
      await snapshot.load(Owner.required, { reload: true });
      assert.deepEqual(snapshot.required.cached.map((r: Dynamic) => r.id), [1, 2]);
      const nested = Owner.required.owner.objects.load(Owner.name).label("person");
      await snapshot.load(nested);
      assert.equal(snapshot.required.cached[0].person.name, "changed");
      const repeated: unknown[] = [];
      const stop = db.onQuery((event) => repeated.push(event));
      await snapshot.load(nested);
      stop();
      assert.equal(repeated.length, 0);
      const keyed = await Required.objects.using(db).filter(Required.id.eq(1)).load(Required.id).get();
      await Required.objects.using(db).filter(Required.id.eq(1)).update({ ownerId: 2 });
      await keyed.load(Required.owner, Required.ownerId);
      assert.equal(keyed.owner.id, keyed.ownerId);
      assert.equal(keyed.owner.id, 2);
      await Required.objects.using(db).update({ ownerId: 2 });
      await row.load(Required.ownerId, { reload: true });
      assert.throws(() => row.owner, NotLoaded);
      await row.load(Required.owner);
      assert.equal(row.owner.id, 2);
      assert.equal(row.loadOwner, undefined);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

const keySource = `datasource db {
 provider = "postgres"
}
model KParent {
 id Int @id
 at DateTime @unique
 amount Decimal @unique
 num Int @unique
 @@map("ref04k_parents")
}
model KChild {
 id Int @id
 at DateTime
 amount Decimal
 num Int
 byDate KParent @relation(fields: [at], references: [at])
 byDecimal KParent @relation(fields: [amount], references: [amount])
 byInt KParent @relation(fields: [num], references: [num])
 @@map("ref04k_children")
}`;
for (const name of ["byDate", "byDecimal", "byInt"]) {
  test(`${name}: a to-one reference key returns the parent after load() and prefetch()`, async () => {
    const registry = new Registry();
    const m = loads(keySource, { registry }) as Dynamic;
    const db = await connect(DATABASE_URL, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      const { KParent, KChild } = m;
      const values = { at: new Date("2024-01-02T03:04:05.678Z"), amount: new Decimal("1.50"), num: 7 };
      await KParent.objects.using(db).insertMany([{ id: 1, ...values }]);
      await KChild.objects.using(db).insertMany([{ id: 1, ...values }]);
      const loaded = await KChild.objects.using(db).get(KChild.id.eq(1));
      assert.throws(() => loaded[name], NotLoaded);
      await loaded.load(KChild[name]);
      assert.equal(loaded[name].id, 1);
      const rows = await KChild.objects.using(db).all();
      await prefetch(rows, KChild[name], { using: db });
      assert.equal(rows[0][name].id, 1);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
