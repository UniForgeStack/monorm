/** Partition metadata survives registry extension and ORM writes use PostgreSQL routing. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";

const source = `
model PartitionRow {
 id Int @default(autoincrement())
 kind String
 amount Int?
 @@id([id, kind])
 @@map("js_partition_row")
 @@partition(list: [kind])
}
partition js_partition_a {
of = PartitionRow
values = ["a"]
@@check("amount IS NOT NULL AND amount > 0")
}
partition js_partition_other {
of = PartitionRow
default = true
}
`;

test("partitions survive registry extension", () => {
  const registry = new Registry();
  loads("model PartitionBefore {\n id Int @id\n}", { registry });
  loads(source, { registry });
  assert.equal((registry.ir()["partitions"] as unknown[]).length, 2);
  loads("model PartitionExtra {\n id Int @id\n}", { registry });
  assert.equal((registry.ir()["partitions"] as unknown[]).length, 2);
  registry.prepare();
});

const url = process.env["ORM_TEST_DATABASE_URL"];
test("partition routes inserts and validates row moves", { skip: !url }, async () => {
  const registry = new Registry();
  const Row = loads(source, { registry })["PartitionRow"] as any;
  const db = await connect(url!, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    const rows = await Row.objects.using(db).insertMany([
      { kind: "a", amount: 3 }, { kind: "b", amount: null }, { kind: "b", amount: 5 },
    ]).returning();
    await assert.rejects(Row.objects.using(db).insert({ kind: "a", amount: null }));
    await assert.rejects(rows[1].update({ kind: "a" }));
    await rows[2].update({ kind: "a" });
    assert.equal(await rows[2].refresh(), true);
    assert.equal(rows[2].kind, "a");
    assert.equal(await Row.objects.using(db).count(), 3);
  } finally {
    await db.dropTables();
    await db.close();
  }
});
