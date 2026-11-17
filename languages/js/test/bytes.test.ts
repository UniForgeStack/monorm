/** The `Bytes` scalar: values, operators and refusals on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, func, loads, QueryError, Registry } from "../src/index.js";

const SCHEMA = 'model Blob {\n  id Int @id\n  data Bytes?\n  tag String @default("t")\n  @@map("bytes_blob")\n}';
const EVERY_BYTE = Uint8Array.from({ length: 256 }, (_, i) => i);

for (const provider of ["sqlite", "postgresql"]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");

  async function setup(t: { skip(why: string): void }): Promise<{ db: any; Blob: any; qs: any } | undefined> {
    const registry = new Registry();
    const Blob = (loads(`datasource db {\n  provider = "${provider}"\n}\n${SCHEMA}`, { registry }) as any)["Blob"];
    let db: any;
    try {
      db = await connect(url, { registry, default: false });
    } catch (error) {
      if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
      t.skip("no reachable database");
      return undefined;
    }
    await db.dropTables();
    await db.createTables();
    return { db, Blob, qs: Blob.objects.using(db) };
  }

  async function withBlob(t: any, run: (w: { Blob: any; qs: any }) => Promise<void>): Promise<void> {
    const w = await setup(t);
    if (w === undefined) return;
    try {
      await run(w);
    } finally {
      await w.db.dropTables();
      await w.db.close();
    }
  }

  test(`bytes round trip (${provider})`, async (t) => {
    await withBlob(t, async ({ Blob, qs }) => {
      const rows = [new Uint8Array(0), Uint8Array.of(0), EVERY_BYTE, new TextEncoder().encode("café"), null];
      await qs.insertMany(rows.map((data, i) => ({ id: i + 1, data })));
      const got = await qs.orderBy(Blob.id).all();
      assert.deepEqual(got.map((r: any) => r.data), rows);
      for (const r of got) if (r.data !== null) assert.ok(r.data instanceof Uint8Array);
    });
  });

  test(`a value that is not bytes is a type error (${provider})`, async (t) => {
    await withBlob(t, async ({ qs }) => {
      for (const bad of ["text", 5, [1, 2], { a: 1 }]) {
        await assert.rejects(qs.insertMany([{ id: 1, data: bad }]), /Uint8Array|bytes/i);
      }
    });
  });

  test(`equality, in and null checks (${provider})`, async (t) => {
    await withBlob(t, async ({ Blob, qs }) => {
      await qs.insertMany([
        { id: 1, data: Uint8Array.of(97) },
        { id: 2, data: Uint8Array.of(98) },
        { id: 3, data: Uint8Array.of(0) },
        { id: 4, data: null },
      ]);
      const ids = async (cond: any): Promise<number[]> => (await qs.filter(cond).select({ id: Blob.id }).all()).map((r: any) => Number(r.id)).sort();
      const d = Blob.data;
      assert.deepEqual(await ids(d.eq(Uint8Array.of(97))), [1]);
      assert.deepEqual(await ids(d.ne(Uint8Array.of(97)).andNotNull()), [2, 3]);
      assert.deepEqual(await ids(d.in([Uint8Array.of(97), Uint8Array.of(0)])), [1, 3]);
      assert.deepEqual(await ids(d.notIn([Uint8Array.of(97), Uint8Array.of(0)]).andNotNull()), [2]);
      assert.deepEqual(await ids(d.isNull()), [4]);
      assert.deepEqual(await ids(d.isNotNull()), [1, 2, 3]);
    });
  });

  test(`bulk update and update write bytes (${provider})`, async (t) => {
    await withBlob(t, async ({ Blob, qs }) => {
      await qs.insertMany([1, 2, 3].map((id) => ({ id, data: Uint8Array.of(120) })));
      await qs.filter(Blob.id.in([1, 2])).update({ data: Uint8Array.of(1, 2) });
      const rows = await qs.orderBy(Blob.id).all();
      assert.deepEqual(rows.map((r: any) => Array.from(r.data)), [[1, 2], [1, 2], [120]]);
      await qs.filter(Blob.id.eq(3)).update({ data: EVERY_BYTE });
      assert.deepEqual((await qs.get(Blob.id.eq(3))).data, EVERY_BYTE);
    });
  });

  test(`order, range, like and aggregates are refused (${provider})`, async (t) => {
    await withBlob(t, async ({ Blob, qs }) => {
      await qs.insertMany([{ id: 1, data: Uint8Array.of(97) }, { id: 2, data: null }]);
      const d = Blob.data;
      const refused = (e: unknown): boolean => e instanceof QueryError && /Bytes/.test(e.message);
      for (const make of [() => d.lt(Uint8Array.of(97)), () => d.gte(Uint8Array.of(97)), () => d.like("a%"), () => d.startsWith("a")]) {
        await assert.rejects(async () => qs.filter(make()).select({ id: Blob.id }).all(), refused);
      }
      await assert.rejects(async () => qs.orderBy(d).all(), refused);
      for (const fn of [func.max, func.min, func.sum, func.avg]) {
        await assert.rejects(async () => qs.select({ v: (fn as any)(d) }).all(), refused);
      }
      assert.deepEqual((await qs.select({ n: func.count(d) }).all()).map((r: any) => Number(r.n)), [1]);
    });
  });
}
