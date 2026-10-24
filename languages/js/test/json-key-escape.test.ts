/** JSON path keys are written into the SQL text: hostile keys must stay inside one literal. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { QueryError, Registry, connect, func, loads } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
model KeyDoc {
  id    BigInt @id @default(autoincrement())
  title String
  meta  Json
  @@map("json_key_docs_js")
}
`;
const KEYS = ["a'); DROP TABLE json_key_docs_js;--\\", "$1", "it's", "back\\slash", "--", ";", "line\nbreak", "\\'", "$", "'"];

for (const setting of ["on", "off"]) {
  test(`hostile JSON keys filter, select and group (standard_conforming_strings ${setting})`, async () => {
    const registry = new Registry();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const Doc = loads(SCHEMA, { registry })["KeyDoc"] as any;
    const db = await connect(DATABASE_URL, { default: false, registry, maxConnections: 1 });
    try {
      await db.execute(`SET standard_conforming_strings = ${setting}`);
      await db.dropTables();
      await db.createTables();
      const objects = Doc.objects.using(db);
      await objects.insertMany([
        ...KEYS.map((k, i) => ({ title: `hit${i}`, meta: { [k]: `v${i}` } })),
        { title: "decoy", meta: { other: "v0" } },
      ]);
      const current = () => db.fetch("SELECT current_setting('standard_conforming_strings') AS v");
      assert.deepEqual(await current(), [{ v: setting }]);
      const events: unknown[] = [];
      const stop = db.onQuery((e) => events.push(e));
      for (const [i, key] of KEYS.entries()) {
        events.length = 0;
        const found = await objects.filter(Doc.meta.get(key).asText().eq(`v${i}`)).all();
        const picked = await objects.filter(Doc.title.eq(`hit${i}`)).select({ v: Doc.meta.get(key).asText() }).all();
        const grouped = await objects
          .filter(Doc.meta.get(key).asText().isNotNull())
          .select({ v: Doc.meta.get(key).asText(), n: func.count() })
          .groupBy(Doc.meta.get(key).asText())
          .all();
        assert.deepEqual(found.map((d: { title: string }) => d.title), [`hit${i}`], key);
        assert.deepEqual(picked.map((r: { v: string }) => r.v), [`v${i}`], key);
        assert.deepEqual(grouped.map((r: { v: string; n: unknown }) => [r.v, Number(r.n)]), [[`v${i}`, 1]], key);
        assert.equal(events.length, 3, key);
      }
      stop();
      assert.deepEqual(await current(), [{ v: setting }]);
      assert.equal(await objects.count(), KEYS.length + 1);
      await assert.rejects(objects.filter(Doc.meta.get("a\0b").asText().eq("x")).count(), (e: unknown) => e instanceof QueryError && /NUL/.test(e.message));
    } finally {
      await db.dropTables();
      await db.close();
    }
  });
}

test("SQLite rejects JSON paths", async () => {
  const registry = new Registry();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const Doc = loads(`datasource db {\n  provider = "sqlite"\n}\n${SCHEMA}`, { registry })["KeyDoc"] as any;
  const db = await connect("sqlite://:memory:", { default: false, registry });
  try {
    await db.createTables();
    await assert.rejects(Doc.objects.using(db).filter(Doc.meta.get("it's").asText().eq("x")).count(), /sqlite does not support JSON paths/);
  } finally {
    await db.close();
  }
});
