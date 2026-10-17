/** Presence and uniqueness are independent constraints on query results. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, DoesNotExist, loads, MultipleObjectsReturned, param, Registry } from "../src/index.js";

async function fixture() {
  const registry = new Registry();
  const Note = loads(`
    datasource db { provider = "sqlite" }
    model Note {
      id Int @id
      n Int?
    }
  `, { registry })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  await Note.objects.using(db).insertMany([{ id: 1, n: null }, { id: 2, n: 5 }]);
  return { db, Note, qs: Note.objects.using(db) };
}

test("optional and required model reads", async () => {
  const { db, Note, qs } = await fixture();
  try {
    const empty = qs.filter(Note.id.eq(-1));
    assert.equal(await empty.get(), null);
    assert.equal(await empty.first(), null);
    assert.equal(await empty.last(), null);
    assert.deepEqual(await empty.all(), []);
    assert.deepEqual(await empty, []);
    const required = empty.require();
    for (const method of ["get", "first", "last", "all"]) {
      await assert.rejects(required[method](), Note.DoesNotExist);
    }
    for (let i = 0; i < 2; i++) await assert.rejects(async () => await required, Note.DoesNotExist);
    await assert.rejects(async () => { for await (const row of required) void row; }, Note.DoesNotExist);
    assert.equal(await empty.get(), null);
    await assert.rejects(qs.require().filter(Note.id.eq(-1)).get(), Note.DoesNotExist);
    assert.equal((await qs.require().get(Note.id.eq(1))).id, 1);
    assert.equal((await qs.get(Note.id.eq(1))).id, 1);
    assert.equal((await qs.orderBy(Note.id).require().first()).id, 1);
    assert.equal((await qs.orderBy(Note.id).require().last()).id, 2);
    assert.equal((await qs.require().all()).length, 2);
    for (const query of [qs, qs.require()]) await assert.rejects(query.get(), MultipleObjectsReturned);
    assert.equal(await required.count(), 0);
    assert.equal(await required.exists(), false);
  } finally { await db.close(); }
});

test("optional and required selected rows", async () => {
  const { db, Note, qs: models } = await fixture();
  try {
    const qs = models.select({ n: Note.n });
    const empty = qs.filter(Note.id.eq(-1));
    assert.equal(await empty.get(), null);
    assert.equal(await empty.first(), null);
    assert.deepEqual(await empty.all(), []);
    for (const method of ["get", "first", "all", "scalar", "scalars"]) {
      await assert.rejects(empty.require()[method](), DoesNotExist);
    }
    await assert.rejects(async () => await empty.require(), DoesNotExist);
    assert.equal(await empty.get(), null);
    for (const query of [qs, qs.require()]) await assert.rejects(query.get(), MultipleObjectsReturned);
    assert.deepEqual(await qs.require().get(Note.id.eq(1)), { n: null });
    assert.deepEqual(await qs.orderBy(Note.id).require().first(), { n: null });
    assert.deepEqual(await qs.orderBy(Note.id).require().all(), [{ n: null }, { n: 5 }]);
    assert.deepEqual(await models.require().filter(Note.id.eq(1)).select({ n: Note.n }).get(), { n: null });
    assert.equal(await qs.filter(Note.id.eq(1)).require().scalar(), null);
  } finally { await db.close(); }
});

test("required prepared queries", async () => {
  const { db, Note, qs: models } = await fixture();
  try {
    const qs = models.filter(Note.id.eq(param("id")));
    const optional = qs.prepare();
    assert.equal(await optional.get({ id: -1 }), null);
    assert.equal(await optional.first({ id: -1 }), null);
    assert.deepEqual(await optional.all({ id: -1 }), []);
    for (const required of [optional.require(), qs.require().prepare()]) {
      assert.equal((await required.get({ id: 1 })).id, 1);
      assert.equal((await required.first({ id: 1 })).id, 1);
      assert.equal((await required.all({ id: 1 })).length, 1);
      for (const method of ["get", "first", "all"]) {
        await assert.rejects(required[method]({ id: -1 }), Note.DoesNotExist);
      }
    }
    const multiple = models.prepare();
    for (const query of [multiple, multiple.require()]) await assert.rejects(query.get(), MultipleObjectsReturned);
    assert.equal(await optional.get({ id: -1 }), null);
  } finally { await db.close(); }
});

test("required one-row queries: at(), lazy and prepared", async () => {
  const { db, Note, qs } = await fixture();
  try {
    const required = qs.require();
    assert.equal((await required.orderBy(Note.id).at(1)).id, 2);
    assert.equal((await required.orderBy(Note.id).at(-1)).id, 2);
    assert.equal(await qs.orderBy(Note.id).at(5), null);
    await assert.rejects(required.at(5), Note.DoesNotExist);
    await assert.rejects(required.filter(Note.id.eq(-1)).at(0), Note.DoesNotExist);
    const lazy = required.at(5);
    await new Promise((resolve) => setTimeout(resolve, 10));
    await assert.rejects(lazy, Note.DoesNotExist);
    const byId = qs.filter(Note.id.eq(param("id"))).first().prepare();
    assert.equal(await byId.run({ id: -1 }), null);
    await assert.rejects(qs.require().filter(Note.id.eq(param("id"))).first().prepare().run({ id: -1 }), Note.DoesNotExist);
  } finally { await db.close(); }
});
