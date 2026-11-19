/** Partition metadata survives registry extension and ORM writes use PostgreSQL routing. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, IntegrityError, loads, Registry, scope } from "../src/index.js";
import { Decimal } from "../src/decimal.js";

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

const url = (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");
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
    await rows[2].load({ reload: true });
    assert.equal(rows[2].kind, "a");
    assert.equal(await Row.objects.using(db).count(), 3);
  } finally {
    await db.dropTables();
    await db.close();
  }
});

import { readFileSync } from "node:fs";
import { hasExtension } from "./artifact-caps.js";
const proxyEnabled = hasExtension("monorm.proxy");
test("proxy partition fills its key and keeps its predicate without defaults", { skip: !url || !proxyEnabled }, async () => {
  const registry = new Registry();
  const models = loads(readFileSync("../../fixtures/partition-proxy.prisma", "utf8").replaceAll("partition_proxy_", "partition_proxy_js_"), { registry });
  const Reward = models.Reward as any, Percent = models.PercentageReward as any, Fixed = models.FixedReward as any;
  const db = await connect(url!, { registry, default: false });
  try {
    await db.dropTables(); await db.createTables();
    const row = await Percent.objects.using(db).insert({ shop: 1, percent: 10 });
    assert.deepEqual(row.pk, { id: row.id, kind: "percent" });
    await Fixed.objects.using(db).insert({ shop: 1, amount: 20 });
    assert.equal(await Reward.objects.using(db).count(), 2);
    assert.equal(await Percent.objects.using(db).withoutDefaults().count(), 1);
    assert.match(Percent.objects.using(db).withoutDefaults().sql(), / IN \('percent'\)/);
    await assert.rejects(Percent.objects.using(db).insert({ shop: 1, percent: null }));
    await assert.rejects(Percent.objects.using(db).insert({ shop: 1, percent: 10, kind: "fixed" }));
    assert.equal(await Percent.objects.using(db).withoutDefaults().all().update({ percent: 15 }), 1);
    assert.equal(await Percent.objects.using(db).withoutDefaults().all().delete(), 1);
    assert.equal(await Reward.objects.using(db).count(), 1);
  } finally { await db.dropTables(); await db.close(); }
});

// case: partition::rewards_proxy_partition_inherits_scope_and_soft_delete
test("rewards proxy partition inherits scope and soft delete", { skip: !url || !hasExtension("monorm.proxy", "monorm.query-defaults", "monorm.soft-delete") }, async () => {
  const text = readFileSync("../../fixtures/partition-proxy.prisma", "utf8")
    .replaceAll("partition_proxy_", "partition_proxy_js2_")
    .replace("deleted DateTime?", '@@soft_delete.deleted_at("deleted")')
    .replace('@@map("partition_proxy_js2_reward")', '@@map("partition_proxy_js2_reward")\n @@query.filter("tenant", (shop == scope.shop) & (deleted == null))');
  const registry = new Registry();
  const models = loads(text, { registry });
  const Reward = models.Reward as any, Percent = models.PercentageReward as any, Fixed = models.FixedReward as any;
  const db = await connect(url!, { registry, default: false });
  try {
    await db.dropTables(); await db.createTables();
    await Percent.objects.using(db).insertMany([{ shop: 1, percent: 10 }, { shop: 2, percent: 20 }]);
    await Fixed.objects.using(db).insert({ shop: 1, amount: 5 });
    await scope({ shop: 1 }, async () => {
      assert.equal(await Percent.objects.using(db).count(), 1);
      assert.equal(await Reward.objects.using(db).count(), 2);
      assert.equal(await Percent.objects.using(db).all().delete(), 1);
      assert.equal(await Percent.objects.using(db).count(), 0);
      assert.equal(await Fixed.objects.using(db).count(), 1);
    });
    // Neither scope nor the deleted-row default remains; storage ownership does.
    assert.equal(await Percent.objects.using(db).withoutDefaults().count(), 2);
    // The proxy has the soft-delete members of its root.
    assert.equal(await Percent.objects.using(db).withoutDefaults().deletedOnly().count(), 1);
  } finally { await db.dropTables(); await db.close(); }
});

// case: partition::multi_value_proxy_partition_requires_a_key_and_owns_only_its_values
test("multi value proxy partition requires a key and owns only its values", { skip: !url || !proxyEnabled }, async () => {
  const text = `
model Item {
 id Int
 kind String
 @@id([id, kind])
 @@map("multi_partition_item_js")
 @@partition(list: [kind])
}
model Selected {
 @@proxy.of(Item)
 @@partition.values(["a", "b"])
 @@map("multi_partition_selected_js")
}
partition multi_partition_other_js { of = Item default = true }
`;
  const registry = new Registry();
  const models = loads(text, { registry });
  const Item = models.Item as any, Selected = models.Selected as any;
  const db = await connect(url!, { registry, default: false });
  try {
    await db.dropTables(); await db.createTables();
    await Selected.objects.using(db).insertMany([{ id: 1, kind: "a" }, { id: 1, kind: "b" }]);
    await Item.objects.using(db).insert({ id: 1, kind: "other" });
    assert.equal(await Selected.objects.using(db).withoutDefaults().count(), 2);
    assert.equal(await Item.objects.using(db).count(), 3);
    await assert.rejects(Selected.objects.using(db).insert({ id: 2 }), /Selected\.kind is required/);
    await assert.rejects(Selected.objects.using(db).insert({ id: 2, kind: "other" }), IntegrityError);
    await assert.rejects(Selected.objects.using(db).insertMany([{ id: 2, kind: "other" }], { copy: true }), IntegrityError);
    assert.equal(await Selected.objects.using(db).insertMany([{ id: 2, kind: "a" }, { id: 2, kind: "b" }], { copy: true }), 2);
    assert.equal(await Selected.objects.using(db).withoutDefaults().count(), 4);
  } finally { await db.dropTables(); await db.close(); }
});

// case: partition::spec_rewards_example_with_decimals_local_uniqueness_and_soft_delete
test("spec rewards example with decimals, local uniqueness and soft delete", { skip: !url || !hasExtension("monorm.proxy", "monorm.query-defaults", "monorm.soft-delete") }, async () => {
  const registry = new Registry();
  const text = readFileSync("../../fixtures/partition-rewards.prisma", "utf8").replaceAll("partition_example_", "partition_example_js_");
  const models = loads(text, { registry });
  const Tier = models.Tier as any, Reward = models.Reward as any, Percent = models.PercentageDiscountReward as any;
  const Fixed = models.FixedDiscountReward as any, Price = models.SetPriceReward as any;
  const db = await connect(url!, { registry, default: false });
  try {
    await db.dropTables(); await db.createTables();
    await Tier.objects.using(db).insert({ id: 1 });
    await scope({ shop: 1 }, async () => {
      const percent = await Percent.objects.using(db).insert({ shopId: 1, tierId: 1, contentHash: "same", percent: new Decimal("10.25") });
      await Fixed.objects.using(db).insert({ shopId: 1, tierId: 1, contentHash: "same", amount: new Decimal("2.50") });
      await Price.objects.using(db).insert({ shopId: 1, tierId: 1, contentHash: "same", price: new Decimal("3.75") });
      assert.equal(percent.percent.toString(), "10.25");
      assert.equal(await Reward.objects.using(db).count(), 3);
      assert.equal(await Percent.objects.using(db).count(), 1);
      await assert.rejects(Percent.objects.using(db).insert({ shopId: 1, tierId: 1, contentHash: "same", percent: new Decimal("12") }), IntegrityError);
      await assert.rejects(Reward.objects.using(db).insert({ shopId: 1, tierId: 1, type: "PERCENTAGE_DISCOUNT", percent: null }), IntegrityError);
      assert.equal(await Percent.objects.using(db).all().delete(), 1);
      assert.equal(await Percent.objects.using(db).count(), 0);
      assert.equal(await Percent.objects.using(db).withoutDefaults().count(), 1);
      await Percent.objects.using(db).insert({ shopId: 1, tierId: 1, contentHash: "same", percent: new Decimal("20") });
      assert.equal(await Reward.objects.using(db).count(), 3);
    });
    await scope({ shop: 2 }, async () => {
      assert.equal(await Reward.objects.using(db).count(), 0);
      assert.equal(await Percent.objects.using(db).count(), 0);
    });
    assert.equal(await Reward.objects.using(db).withoutDefaults().count(), 4);
  } finally { await db.dropTables(); await db.close(); }
});
