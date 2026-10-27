/** `of()` applies no query default on the root of a bound read or write; a relation keeps the default of its target. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, func, loads, of, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const source = `
model BqOwner {
  id     Int     @id
  name   String
  active Boolean @default(true)
  kids   BqKid[]
  @@map("bqo_owner")
  @@query.filter("active", "active == true")
}

model BqKid {
  id      Int     @id
  ownerId Int
  active  Boolean @default(true)
  owner   BqOwner @relation(fields: [ownerId], references: [id])
  @@map("bqo_kid")
  @@query.filter("active", "active == true")
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
const skip = !hasExtension("orm.query-defaults");

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"];

  async function setup() {
    const registry = new Registry();
    const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${source}`, { registry }) as Record<string, Any>;
    const db = await connect(url!, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    const { BqOwner: Owner, BqKid: Kid } = models;
    await Owner.objects.using(db).insertMany([{ id: 1, name: "shown", active: true }, { id: 2, name: "hidden", active: false }]);
    await Kid.objects.using(db).withoutDefaults().insertMany([
      { id: 1, ownerId: 1, active: true },
      { id: 2, ownerId: 1, active: false },
      { id: 3, ownerId: 2, active: true },
    ]);
    const held = await Owner.objects.using(db).withoutDefaults().orderBy("id");
    return { db, Owner, Kid, held };
  }

  test(`a bound read ignores the root default and a filter still narrows (${provider})`, { skip: skip || !url }, async () => {
    const { db, Owner, held } = await setup();
    try {
      assert.equal(await Owner.objects.using(db).count(), 1);
      assert.deepEqual((await Owner.objects.of(held).using(db)).map((o: Any) => o.id), [1, 2]);
      assert.equal(await Owner.objects.of(held).using(db).count(), 2);
      assert.deepEqual((await Owner.objects.of(held).using(db).filter(Owner.id.eq(2))).map((o: Any) => o.id), [2]);
      assert.deepEqual((await held[1].qs().using(db)).map((o: Any) => o.id), [2]);
      assert.deepEqual((await of(held[1]).using(db)).map((o: Any) => o.id), [2]);
    } finally {
      await db.close();
    }
  });

  test(`a bound write ignores the root default (${provider})`, { skip: skip || !url }, async () => {
    const { db, Owner, Kid, held } = await setup();
    try {
      assert.equal((await Owner.objects.of(held).using(db).update({ name: "changed" })).length, 2);
      assert.deepEqual(held.map((o: Any) => o.name), ["changed", "changed"]);
      assert.equal((await Owner.objects.of(held[1]).using(db).filter(Owner.id.eq(1)).update({ name: "narrow" })).length, 0);
      await Kid.objects.using(db).withoutDefaults().filter(Kid.id.eq(3)).delete();
      assert.equal((await Owner.objects.of(held[1]).using(db).delete()).length, 1);
      const rest = await Owner.objects.using(db).withoutDefaults();
      assert.deepEqual(rest.map((o: Any) => o.id), [1]);
    } finally {
      await db.close();
    }
  });

  test(`a bound aggregate keeps the default of its target (${provider})`, { skip: skip || !url }, async () => {
    const { db, Owner, held } = await setup();
    try {
      await Owner.objects.of(held).using(db).load(func.count(Owner.kids).label("n"));
      assert.deepEqual(held.map((o: Any) => [o.id, Number(o.n)]), [[1, 1], [2, 1]]);
    } finally {
      await db.close();
    }
  });

  test(`an instance label load keeps the default of its target (${provider})`, { skip: skip || !url }, async () => {
    const { db, Owner, held } = await setup();
    try {
      const n = func.count(Owner.kids).label("n");
      await held[0].load(n);
      await held[1].load(n);
      assert.deepEqual([Number(held[0].n), Number(held[1].n)], [1, 1]);
    } finally {
      await db.close();
    }
  });

  test(`an instance label is read again on each load (${provider})`, { skip: skip || !url }, async () => {
    const { db, Owner, Kid, held } = await setup();
    try {
      const n = func.count(Owner.kids).label("n");
      await held[0].load(n);
      assert.equal(Number(held[0].n), 1);
      await Kid.objects.using(db).withoutDefaults().insertMany([{ id: 4, ownerId: 1, active: true }]);
      await held[0].load(n);
      assert.equal(Number(held[0].n), 2);
    } finally {
      await db.close();
    }
  });
}
