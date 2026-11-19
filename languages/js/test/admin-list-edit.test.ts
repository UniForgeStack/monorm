// Edit in the list (`list.editable`): the host dispatch of one save of many rows, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, type ResourceConfig } from "../src/admin.js";
import { connect, load, registerFieldAdapter, Registry, type Database } from "../src/index.js";
import { ValidationError } from "../src/errors.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model LNote {
  id    Int    @id @default(autoincrement())
  title String @db.VarChar(20)
  slug  String @default("")
  @@map("l_notes")
}
`;

const NOTES = "/admin/api/resources/LNote";
const SU = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-list-edit-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), SCHEMA(provider));
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
  let db: Database;
  try {
    db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
    return;
  }
  try {
    await db.dropTables();
    await db.createTables();
    await models["LNote"].objects.using(db).insertMany([{ title: "One" }, { title: "Two" }, { title: "Three" }]);
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

function clientOf(w: World, config: ResourceConfig = {}) {
  const Note = w.models["LNote"];
  const site = new Admin(w.registry);
  site.register(Note, { list: { columns: ["title", "slug"], editable: [Note.title, Note.slug], order: [Note.id] }, ...config } as ResourceConfig);
  return site.testClient(w.db, { user: SU });
}

async function titles(w: World): Promise<string[]> {
  const Note = w.models["LNote"];
  return (await Note.objects.using(w.db).orderBy(Note.id)).map((r: Loose) => r.title);
}

const rows = (...pairs: [number, string, string][]) => ({ rows: pairs.map(([key, set, loaded]) => ({ key: String(key), set: { title: set }, loaded: { title: loaded } })) });

const lower = {
  validate: (value: string) => value.toLowerCase(),
  prepare: async (value: string) => value,
  decode: (stored: string) => stored,
};

for (const provider of PROVIDERS) {
  test(`the list names its editable columns and a wrong name is a build error (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["LNote"];
    const r = await clientOf(w).get("/admin/api/resources");
    const meta = r.json().resources.find((res: Loose) => res.name === "LNote");
    assert.deepEqual(meta.editable, ["title", "slug"]);
    const site = new Admin(w.registry);
    site.register(Note, { list: { columns: ["title"], editable: [Note.slug] } } as ResourceConfig);
    assert.throws(() => site.check(), (e: unknown) => e instanceof AdminConfigError && /not a column of the list/.test(e.message));
  }));

  test(`the default writer saves the rows of a list in one request and a failing row rolls all back (${provider})`, () => world(provider, async (w) => {
    const client = clientOf(w);
    const r = await client.patch(NOTES, { json: rows([1, "Uno", "One"], [3, "Tres", "Three"]) });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(await titles(w), ["Uno", "Two", "Tres"]);
    const bad = await client.patch(NOTES, { json: rows([1, "Eins", "Uno"], [2, "x".repeat(21), "Two"]) });
    assert.equal(bad.status, 400, bad.text);
    assert.deepEqual(await titles(w), ["Uno", "Two", "Tres"]);
  }));

  test(`a service runs once per changed row in one transaction and a failure in row three rolls back rows one and two (${provider})`, () => world(provider, async (w) => {
    const seen: number[] = [];
    const fired: number[] = [];
    const client = clientOf(w, {
      async update(ctx, qs, changes) {
        const row = await qs.first();
        seen.push(row.id);
        if (changes["title"] === "boom") throw new ValidationError({ title: "No boom." });
        await ctx.database!.transaction(async () => {
          await qs.update({ title: changes["title"] });
        });
        await ctx.database!.onCommit(() => { fired.push(row.id); });
      },
    });
    const r = await client.patch(NOTES, { json: rows([1, "Uno", "One"], [2, "Two", "Two"], [3, "Tres", "Three"]) });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [1, 3]);
    assert.deepEqual(await titles(w), ["Uno", "Two", "Tres"]);
    assert.deepEqual(fired, [1, 3]);
    seen.length = 0;
    fired.length = 0;
    const bad = await client.patch(NOTES, { json: rows([1, "Eins", "Uno"], [2, "Zwei", "Two"], [3, "boom", "Tres"]) });
    assert.equal(bad.status, 400, bad.text);
    assert.deepEqual(bad.json().rows["2"].fields["title"], ["No boom."]);
    assert.deepEqual(seen, [1, 2, 3]);
    assert.deepEqual(await titles(w), ["Uno", "Two", "Tres"]);
    assert.deepEqual(fired, []);
  }));

  test(`a changed row answers 409 by row and the save writes nothing (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["LNote"];
    const client = clientOf(w);
    await Note.objects.using(w.db).filter(Note.id.eq(2)).update({ title: "Dos" });
    const r = await client.patch(NOTES, { json: rows([1, "Uno", "One"], [2, "Zwei", "Two"]) });
    assert.equal(r.status, 409, r.text);
    assert.deepEqual(r.json().conflicts, { "1": { title: { loaded: "Two", current: "Dos" } } });
    assert.deepEqual(await titles(w), ["One", "Dos", "Three"]);
  }));

  test(`a model with host behavior saves through the default writer in one transaction (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["LNote"];
    registerFieldAdapter(Note._meta, "slug", lower);
    const client = clientOf(w);
    const body = { rows: [{ key: "1", set: { slug: "AbC" }, loaded: { slug: "" } }, { key: "2", set: { title: "Zwei" }, loaded: { title: "Two" } }] };
    const r = await client.patch(NOTES, { json: body });
    assert.equal(r.status, 200, r.text);
    const all = await Note.objects.using(w.db).orderBy(Note.id);
    assert.deepEqual(all.map((x: Loose) => [x.title, x.slug]), [["One", "abc"], ["Zwei", ""], ["Three", ""]]);
    const Entry = w.models["AdminLogEntry"];
    const entries = async () => (await Entry.objects.using(w.db).orderBy(Entry.id)).map((x: Loose) => [x.action, x.recordKey]);
    assert.deepEqual(await entries(), [["update", "1"], ["update", "2"]]);
    const bad = await client.patch(NOTES, { json: rows([1, "Uno", "One"], [3, "x".repeat(21), "Three"]) });
    assert.equal(bad.status, 400, bad.text);
    assert.equal(bad.json().rows["3"], undefined);
    assert.ok(bad.json().rows["1"].fields["title"]);
    const stale = await client.patch(NOTES, { json: rows([1, "Uno", "One"], [2, "Dos", "Old"]) });
    assert.equal(stale.status, 409, stale.text);
    assert.deepEqual(stale.json().conflicts, { "1": { title: { loaded: "Old", current: "Zwei" } } });
    assert.deepEqual(await titles(w), ["One", "Zwei", "Three"]);
    assert.equal((await entries()).length, 2);
  }));
}
