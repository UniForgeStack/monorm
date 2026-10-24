/** Same-path update aggregates share a row subquery on both databases. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, func, loads, Registry, statementInspection } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const schema = `
model Parent {
  id Int @id
  n Int @default(0)
  total BigInt?
  distinct_n Int @default(0)
  filtered_total BigInt?
  other_n Int @default(0)
  label String @default("old")
  children Child[]
  others Other[]
  @@map("ua_node_parent")
}
model Child {
  id Int @id
  parent_id Int
  parent Parent @relation(fields: [parent_id], references: [id])
  views Int
  @@map("ua_node_child")
}
model Other {
  id Int @id
  parent_id Int
  parent Parent @relation(fields: [parent_id], references: [id])
  @@map("ua_node_other")
}
`;

for (const provider of ["sqlite", "postgresql"]) {
  test(`update aggregates share only their relation path (${provider})`, async () => {
    const registry = new Registry();
    const models = loads(`datasource db {\n provider = "${provider}"\n}\n${schema}`, { registry });
    const Parent: any = models["Parent"], Child: any = models["Child"], Other: any = models["Other"];
    const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : DATABASE_URL, { registry, default: false });
    try {
      await db.dropTables();
      await db.createTables();
      await Parent.objects.using(db).insertMany([{ id: 1 }, { id: 2 }, { id: 3 }]);
      await Child.objects.using(db).insertMany([
        { id: 1, parentId: 1, views: 5 }, { id: 2, parentId: 1, views: 5 },
        { id: 3, parentId: 1, views: 20 }, { id: 4, parentId: 2, views: 7 },
      ]);
      await Other.objects.using(db).insertMany([{ id: 1, parentId: 1 }, { id: 2, parentId: 2 }]);
      const query = Parent.objects.using(db).filter(Parent.id.gt(0)).update({
        n: func.count(Parent.children), label: "updated", total: func.sum(Parent.children.views),
        distinctN: func.count(Parent.children.views, { distinct: true }),
        filteredTotal: func.sum(Parent.children.views, { filter: Parent.children.views.gt(10) }),
        otherN: func.count(Parent.others),
      }, { returning: true });
      assert.deepStrictEqual(query.inspect().statements, [statementInspection({ scalarCorrelated: 2 })]);
      const statements: string[] = [];
      const remove = db.onQuery((e) => { statements.push(e.sql); });
      const rows = (await query).sort((a: any, b: any) => a.id - b.id);
      remove();
      assert.equal(statements.length, 1);
      const sql = statements[0]!;
      assert.ok(sql.includes('SET ("n", "total", "distinct_n", "filtered_total") = (SELECT'), sql);
      assert.equal(sql.split("SELECT").length - 1, 2);
      assert.ok(sql.includes("DISTINCT") && sql.includes("FILTER") && !sql.includes("LATERAL"), sql);
      assert.deepEqual(rows.map((r: any) => [r.n, r.total, r.distinctN, r.filteredTotal, r.otherN, r.label]), [
        [3, 30n, 2, 20n, 1, "updated"], [1, 7n, 1, null, 1, "updated"], [0, null, 0, null, 0, "updated"],
      ]);
      const single = Parent.objects.using(db).update({ n: func.count(Parent.children) });
      assert.deepStrictEqual(single.inspect().statements, [statementInspection({ scalarCorrelated: 1 })]);
      statements.length = 0;
      const removeSingle = db.onQuery((e) => { statements.push(e.sql); });
      assert.equal(await single, 3);
      removeSingle();
      assert.ok(statements[0]!.includes('SET "n" = (SELECT'));
      const pair = Parent.objects.using(db).filter(Parent.id.gt(1)).update({ n: func.count(Parent.children), total: func.sum(Parent.children.views) });
      assert.deepStrictEqual(pair.inspect().statements, [statementInspection({ scalarCorrelated: 1 })]);
      assert.equal(await pair, 2);
      const pairedRows = await Parent.objects.using(db).orderBy(Parent.id).all();
      assert.deepEqual(pairedRows.map((r: any) => [r.n, r.total]), [[3, 30n], [1, 7n], [0, null]]);
      const wrapped = Parent.objects.using(db).update({
        n: func.count(Parent.children).add(1), total: func.sum(Parent.children.views),
      }, { returning: true });
      assert.equal(wrapped.inspect().statements[0]!.scalarCorrelated, 1);
      statements.length = 0;
      const removeWrapped = db.onQuery((e) => { statements.push(e.sql); });
      const wrappedRows = (await wrapped).sort((a: any, b: any) => a.id - b.id);
      removeWrapped();
      assert.deepEqual(wrappedRows.map((r: any) => [r.n, r.total]), [[4, 30n], [2, 7n], [1, null]]);
      assert.equal(statements[0]!.split('FROM "ua_node_child"').length - 1, 1);
      const combined = Parent.objects.using(db).filter(Parent.id.gt(0)).update({
        n: func.case(
          [func.count(Parent.children).gt(0), func.count(Parent.children).add(func.count(Parent.others))],
          { default: 99 },
        ),
        total: func.sum(Parent.children.views), otherN: func.count(Parent.others).add(1), label: "wrapped",
      }, { returning: true });
      const shape = combined.inspect().statements[0]!;
      assert.equal(shape.scalarCorrelated, 1);
      assert.equal(shape.derivedCorrelated, 2);
      const combinedRows = (await combined).sort((a: any, b: any) => a.id - b.id);
      assert.deepEqual(combinedRows.map((r: any) => [r.n, r.total, r.otherN, r.label]), [
        [4, 30n, 2, "wrapped"], [2, 7n, 2, "wrapped"], [99, null, 1, "wrapped"],
      ]);
      const repeated = Parent.objects.using(db).update({ n: func.count(Parent.children).add(func.count(Parent.children)) }, { returning: true });
      assert.equal(repeated.inspect().statements[0]!.scalarCorrelated, 1);
      const repeatedRows = (await repeated).sort((a: any, b: any) => a.id - b.id);
      assert.deepEqual(repeatedRows.map((r: any) => r.n), [6, 2, 0]);
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}
