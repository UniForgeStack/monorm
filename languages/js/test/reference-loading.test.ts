import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, of, Decimal, Registry, IntegrityError, NotLoaded, type Database } from "../src/index.js";
import { INTERNAL, RELATED } from "../src/model.js";
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
type Refs = {
  db: Database;
  m: Dynamic;
  row: any;
  proto: { first: unknown };
  original: (this: unknown, ...args: unknown[]) => PromiseLike<unknown>;
  /** Counts the `first()` calls the loads make; `calls()` is the count so far. */
  countFirst: () => () => number;
};
/** A fresh database with owners 1 and 2 and the `Required` row 1 that points at owner 1. */
async function withRefs(dialect: string, body: (refs: Refs) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const m = loads(`datasource db {\n provider = "${dialect}"\n}\n${source}`, { registry }) as Dynamic;
  const db = await connect(dialect === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false });
  const proto = QuerySet.prototype as unknown as { first: unknown };
  const original = QuerySet.prototype.first as unknown as Refs["original"];
  await db.dropTables();
  await db.createTables();
  try {
    await m["Owner"].objects.using(db).insertMany([{ id: 1, name: "one" }, { id: 2, name: "two" }]);
    const row = await m["Required"].objects.using(db).insert({ id: 1, ownerId: 1 });
    const countFirst = () => {
      let calls = 0;
      proto.first = async function (this: unknown, ...args: unknown[]) { calls++; return original.apply(this, args); };
      return () => calls;
    };
    await body({ db, m, row, proto, original, countFirst });
  } finally {
    proto.first = original;
    await db.dropTables();
    await db.close();
  }
}
async function queryCount(db: Database, run: () => Promise<unknown>): Promise<number> {
  let n = 0;
  const remove = db.onQuery(() => { n++; });
  try { await run(); } finally { remove(); }
  return n;
}
for (const dialect of ["sqlite", "postgres"]) {
  test(`${dialect}: identity, reload, fk invalidation and counts`, () => withRefs(dialect, async ({ db, m, row, countFirst }) => {
    const { Owner, Required } = m;
    const calls = countFirst();
    assert.throws(() => row.owner, NotLoaded);
    const values = await Promise.all(Array.from({ length: 12 }, () => loadRelation(row, "owner")));
    assert.equal(calls(), 1);
    assert.ok(values.every((value) => value === values[0]));
    assert.equal(row.owner, values[0]);
    assert.equal(await loadRelation(row, "owner"), row.owner);
    assert.equal(calls(), 1);
    await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "changed" });
    const fresh = await loadRelation(row, "owner", true);
    assert.ok(fresh === row.owner && fresh !== values[0] && fresh.name === "changed");
    assert.equal(calls(), 2);
    await row.update({ ownerId: 2 });
    assert.throws(() => row.owner, NotLoaded);
    assert.equal((await loadRelation(row, "owner")).id, 2);
    assert.equal(calls(), 3);
    await Required.objects.using(db).all().update({ ownerId: 1 });
    await row.load({ reload: true });
    assert.throws(() => row.owner, NotLoaded);
    assert.equal((await loadRelation(row, "owner")).id, 1);
    const eager = await Required.objects.using(db).load(Required.owner).get();
    assert.equal(await loadRelation(eager, "owner"), eager.owner);
    assert.equal(calls(), 4);
  }));

  test(`${dialect}: cached absence, refresh, reverse and null`, () => withRefs(dialect, async ({ db, m, countFirst }) => {
    const { Owner, Detail, Optional } = m;
    const owner = await Owner.objects.using(db).filter(Owner.id.eq(1)).get();
    const calls = countFirst();
    assert.equal(await loadRelation(owner, "detail"), null);
    assert.equal(owner.detail, null);
    assert.equal(await loadRelation(owner, "detail"), null);
    assert.equal(calls(), 1);
    await Detail.objects.using(db).insert({ id: 1, ownerId: 1 });
    assert.equal(await loadRelation(owner, "detail"), null);
    await owner.load({ reload: true });
    assert.throws(() => owner.detail, NotLoaded);
    assert.equal((await loadRelation(owner, "detail")).id, 1);
    assert.equal(calls(), 2);
    const nullable = await Optional.objects.using(db).insert({ id: 1, ownerId: null });
    assert.equal(await loadRelation(nullable, "owner"), null);
    assert.equal(calls(), 2);
    assert.equal(await loadRelation(nullable, "owner", true), null);
    assert.equal(calls(), 2);
  }));

  test(`${dialect}: required integrity, optional missing and retry`, () => withRefs(dialect, async ({ db, m, row }) => {
    const nullable = await m["Optional"].objects.using(db).insert({ id: 1, ownerId: 1 });
    // Emulate physically corrupt legacy data without changing server constraints.
    row.ownerId = 999;
    nullable.ownerId = 999;
    await assert.rejects(loadRelation(row, "owner"), (e: Error) => e instanceof IntegrityError && /required target/.test(e.message));
    assert.equal(await loadRelation(nullable, "owner"), null);
    assert.equal(nullable.owner, null);
    assert.equal(await loadRelation(nullable, "owner"), null);
    row.ownerId = 1;
    assert.equal((await loadRelation(row, "owner")).id, 1);
  }));

  test(`${dialect}: transaction and separate database context`, () => withRefs(dialect, async ({ db, m }) => {
    const { Owner, Required } = m;
    const row = await Required.objects.using(db).get();
    await assert.rejects(db.transaction(async () => {
      await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "uncommitted" });
      assert.equal((await loadRelation(row, "owner")).name, "uncommitted");
      throw new Error("rollback");
    }), /rollback/);
    assert.equal((await loadRelation(row, "owner", true)).name, "one");
  }));

  test(`${dialect}: cancelled waiter, failure and inflight refresh`, () => withRefs(dialect, async ({ m, row, proto, original }) => {
    // A JS promise has no cancel: the second waiter joins the in-flight load instead.
    let release!: () => void;
    let start!: () => void;
    const gate = new Promise<void>((done) => { release = done; });
    const started = new Promise<void>((done) => { start = done; });
    let calls = 0;
    proto.first = async function (this: unknown, ...args: unknown[]) {
      calls++;
      const value = await original.apply(this, args);
      start(); await gate;
      return value;
    };
    const one = row.load(m["Required"].owner);
    await started;
    const two = row.load(m["Required"].owner);
    await row.update({ ownerId: 2 });
    release();
    assert.equal(await one, row);
    assert.equal(await two, row);
    assert.throws(() => row.owner, NotLoaded);
    assert.equal((await loadRelation(row, "owner")).id, 2);
    assert.equal(calls, 2);
    proto.first = async function () { throw new Error("temporary failure"); };
    await assert.rejects(loadRelation(row, "owner", true), /temporary/);
    proto.first = original;
    assert.equal((await loadRelation(row, "owner", true)).id, 2);
  }));

  test(`${dialect}: hidden dependency keys and missing key fail before io`, () => withRefs(dialect, async ({ m, row, proto }) => {
    // Helper-only source and target keys remain sufficient for loading without becoming public.
    row[INTERNAL] = { ownerId: row.ownerId };
    delete row.ownerId;
    const owner = await loadRelation(row, "owner");
    assert.equal(row.owner, owner);
    owner[INTERNAL] = { id: owner.id };
    delete owner.id;
    assert.equal(await loadRelation(row, "owner"), owner);
    assert.equal(row.owner, owner);
    assert.equal(owner.id, 1);
    row[INTERNAL] = {};
    proto.first = async function () { throw new Error("must fail before I/O"); };
    await assert.rejects(loadRelation(row, "owner", true), (e: Error) => e instanceof NotLoaded && /ownerId/.test(e.message));
  }));

  test(`${dialect}: update with an unloaded reference key`, () => withRefs(dialect, async ({ row }) => {
    delete row.ownerId;
    await row.update({ ownerId: 2 });
    assert.equal((await loadRelation(row, "owner")).id, 2);
  }));

  test(`${dialect}: different transaction contexts do not coalesce`, () => withRefs(dialect, async ({ db, m, row, proto, original }) => {
    const { Owner, Required } = m;
    let releaseOutside!: () => void;
    let outsideStarted!: () => void;
    const outsideGate = new Promise<void>((done) => { releaseOutside = done; });
    const outsideReady = new Promise<void>((done) => { outsideStarted = done; });
    let calls = 0;
    proto.first = async function (this: unknown, ...args: unknown[]) {
      calls++;
      const value = await original.apply(this, args);
      if (calls === 1) { outsideStarted(); await outsideGate; }
      return value;
    };
    const outside = row.load(Required.owner);
    await outsideReady;
    try {
      await db.transaction(async () => {
        await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "inside" });
        const inside = await loadRelation(row, "owner", true);
        assert.ok(inside.name === "inside" && calls === 2);
        releaseOutside();
        assert.equal(await outside, row);
        assert.equal(row.owner, inside);
      });
    } finally {
      releaseOutside();
      await outside;
    }
  }));

  test(`${dialect}: named shared primary key references`, () => withRefs(dialect, async ({ db, m }) => {
    const role = await m["Role"].objects.using(db).insert({ id: 1 });
    const person = await loadRelation(role, "person");
    assert.ok(person.pk === role.pk && role.person === person);
    const reverse = await loadRelation(person, "role");
    assert.ok(reverse.pk === person.pk && person.role === reverse);
    assert.equal(await loadRelation(reverse, "person"), reverse.person);
  }));
}

test("a set key with no target row gives a NotLoaded that says why", async () => {
  const registry = new Registry();
  const m = loads(`datasource db {\n provider = "sqlite"\n}\n${source}`, { registry }) as Dynamic;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await m["Owner"].objects.using(db).insert({ id: 1, name: "one" });
    const row = await m["Required"].objects.using(db).insert({ id: 1, ownerId: 1 });
    assert.throws(() => row.owner, (e: Error) => e instanceof NotLoaded && !/no row of the target/.test(e.message));
    // Emulate corrupt data: the join found no target for a required key.
    row[RELATED] = { owner: null };
    assert.throws(() => row.owner, (e: Error) => e instanceof NotLoaded && /is not loaded: the key is set and no row of the target has it; .*load\(Required\.owner\)/.test(e.message));
  } finally {
    await db.close();
  }
});

test("load member collision fails atomically", () => {
  const registry = new Registry();
  assert.throws(() => loads(source.replace("owner_id Int\n", "owner_id Int\n load String\n"), { registry }), /reserved/);
  assert.equal([...registry].length, 0);
});

for (const dialect of ["sqlite", "postgres"]) {
  test(`${dialect}: instance load is additive and skips loaded fields`, () => withRefs(dialect, async ({ db, m }) => {
    const { Owner } = m;
    const partial = await Owner.objects.using(db).filter(Owner.id.eq(1)).load(Owner.id).get();
    assert.throws(() => partial.name, NotLoaded);
    const queries = await queryCount(db, async () => {
      assert.equal(await partial.load(Owner.name), partial);
      assert.ok(partial.id === 1 && partial.name === "one");
      await partial.load(Owner.name);
      await partial.load();
    });
    assert.equal(queries, 1);
    await Owner.objects.using(db).filter(Owner.id.eq(1)).update({ name: "changed" });
    await partial.load(Owner.name);
    assert.equal(partial.name, "one");
    await partial.load(Owner.name, { reload: true });
    assert.ok(partial.name === "changed" && partial.id === 1);
    assert.equal(partial.loadDetail, undefined);
  }));

  test(`${dialect}: instance load nested partial and collection queries`, () => withRefs(dialect, async ({ db, m, row }) => {
    const { Owner, Required } = m;
    await row.load(Required.owner.name);
    const owner = row.owner;
    assert.equal(owner.name, "one");
    assert.throws(() => owner.id, NotLoaded);
    await row.load(Required.owner);
    assert.ok(row.owner === owner && owner.id === 1);
    await owner.load(Owner.required.owner.name);
    assert.equal(owner.required.cached[0].owner.name, "one");
    assert.equal(await queryCount(db, () => owner.load(Owner.required.owner.name)), 0);
    await owner.load(Owner.required.objects.filter(Required.id.eq(1)).load(Required.id).label("selected"));
    assert.deepEqual(owner.selected.map((r: Dynamic) => r.id), [1]);
    assert.throws(() => owner.selected[0].ownerId, NotLoaded);
    await owner.load(Owner.optional);
    assert.deepEqual(owner.optional.cached, []);
    assert.equal(await queryCount(db, () => owner.load(Owner.optional)), 0);
  }));

  test(`${dialect}: instance load validates before io and reload invalidates reference`, () => withRefs(dialect, async ({ db, m, row }) => {
    const { Owner, Required } = m;
    assert.equal(await queryCount(db, async () => {
      await assert.rejects(async () => { await row.load(Required.owner, Owner.name); }, TypeError);
    }), 0);
    await row.load(Required.owner);
    await Required.objects.using(db).all().update({ ownerId: 2 });
    await row.load(Required.ownerId, { reload: true });
    assert.throws(() => row.owner, NotLoaded);
    await row.load(Required.owner);
    assert.equal(row.owner.id, 2);
  }));

  test(`${dialect}: loading a key and its reference uses the new key`, () => withRefs(dialect, async ({ db, m }) => {
    const { Required } = m;
    const row = await Required.objects.using(db).load(Required.id).get();
    await Required.objects.using(db).all().update({ ownerId: 2 });
    await row.load(Required.owner, Required.ownerId);
    assert.ok(row.ownerId === row.owner.id && row.owner.id === 2);
  }));

  test(`${dialect}: nested load preserves cached collection membership`, () => withRefs(dialect, async ({ db, m, row }) => {
    const { Owner, Required } = m;
    const owner = await Owner.objects.using(db).filter(Owner.id.eq(1)).get();
    await owner.load(Owner.required.objects.load(Required.id));
    const [cached] = owner.required.cached;
    await Required.objects.using(db).insert({ id: 2, ownerId: 1 });
    await owner.load(Owner.required.owner.name);
    assert.deepEqual(owner.required.cached.map((r: Dynamic) => r.id), [row.id]);
    assert.equal(owner.required.cached[0], cached);
    assert.equal(cached.owner.name, "one");
    const partial = Owner.required.objects.load(Required.id).label("selected");
    await owner.load(partial);
    assert.equal(await queryCount(db, () => owner.load(partial)), 0);
    await owner.load(Owner.required, { reload: true });
    assert.deepEqual(owner.required.cached.map((r: Dynamic) => r.id), [1, 2]);
  }));

  test(`${dialect}: nested relation queryset labels are cached`, () => withRefs(dialect, async ({ db, m }) => {
    const { Owner } = m;
    const owner = await Owner.objects.using(db).filter(Owner.id.eq(1)).get();
    const nested = Owner.required.owner.objects.load(Owner.name).label("person");
    await owner.load(nested);
    assert.equal(owner.required.cached[0].person.name, "one");
    assert.equal(await queryCount(db, () => owner.load(nested)), 0);
  }));
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
  test(`${name}: a to-one reference key returns the parent after load() and a bound load`, async () => {
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
      await of(rows).using(db).load(KChild[name]);
      assert.equal(rows[0][name].id, 1);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

test("the host does not reserve the soft-delete member names", () => {
  const models = loads("model Plain {\n id Int @id\n hardDelete String\n undelete String\n}\n", { registry: new Registry() });
  assert.ok(models.Plain);
});
