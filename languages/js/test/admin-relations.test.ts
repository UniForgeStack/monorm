/** Admin relation fields and files: autocomplete, `choices`, many-to-many chips, upload and download, through `admin.testClient`, on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Operator } from "opendal";

import { Admin, AdminConfigError, type AdminOptions, type AdminResponse, type ResourceConfig } from "../src/admin.js";
import { UserMethods, UserQueries, makePassword } from "../src/auth/index.js";
import { connect, load, Registry, useModelMethods, useQuerySet, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;
const SECRET = "s".repeat(40);
const SU = { key: 1, is_superuser: true };
const API = "/admin/api";
const files = hasExtension("monorm.file-storage");

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:auth" (prefix: "auth_")
import "app:admin" (prefix: "admin_")

model RAuthor {
  id     Int      @id @default(autoincrement())
  name   String
  active Boolean  @default(true)
  books  RBook[]
  @@map("r_authors")
}

model RTag {
  id    Int         @id @default(autoincrement())
  name  String
  books RBook[]     @relation(through: RBookTag)
  links RBookTag[]
  @@map("r_tags")
}

model RBook {
  id         Int         @id @default(autoincrement())
  author_id  Int
  title      String
  cover      Json?
  author     RAuthor     @relation(fields: [author_id], references: [id])
  tags       RTag[]      @relation(through: RBookTag)
  links      RBookTag[]
  @@map("r_books")
}

model RBookTag {
  id      Int   @id @default(autoincrement())
  book_id Int
  tag_id  Int
  book    RBook @relation(fields: [book_id], references: [id], onDelete: Cascade)
  tag     RTag  @relation(fields: [tag_id], references: [id], onDelete: Cascade)
  @@map("r_book_tags")
}
`;

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-relations-"));
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), SCHEMA(provider));
  const registry = new Registry();
  const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
  useModelMethods(models["User"], UserMethods);
  useQuerySet(models["User"], UserQueries);
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
    const using = (name: string) => models[name].objects.using(db);
    await using("User").insertMany([{ username: "ann", email: "ann@example.com", password: makePassword("pw"), isSuperuser: true }]);
    await using("RAuthor").insertMany([{ name: "Ada" }, { name: "Bo", active: false }, ...Array.from({ length: 28 }, (_, i) => ({ name: `Author ${String(i + 3).padStart(2, "0")}` }))]);
    await using("RTag").insertMany(["red", "green", "blue", "grey"].map((name) => ({ name })));
    await using("RBook").insertMany([{ authorId: 1, title: "First" }]);
    await using("RBookTag").insertMany([{ bookId: 1, tagId: 1 }, { bookId: 1, tagId: 2 }]);
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

function siteOf(w: World, book: ResourceConfig = {}, author: ResourceConfig = {}, options: AdminOptions = {}): Admin {
  const site = new Admin(w.registry, options);
  const { RAuthor: a, RTag: t, RBook: b } = w.models;
  site.register(a, { list: { columns: [a.name], order: [a.id], search: [a.name] }, ...author } as never);
  site.register(t, { list: { columns: [t.name], order: [t.id], search: [t.name] } } as never);
  site.register(b, { list: { columns: [b.title], order: [b.id] }, relations: { author: {}, tags: {} }, ...book } as never);
  return site;
}

async function links(w: World, book: number): Promise<number[]> {
  const link = w.models["RBookTag"];
  const rows = await link.objects.using(w.db).filter(link.bookId.eq(book)).orderBy(link.tagId);
  return rows.map((r: Loose) => r.tagId);
}

const audit = (w: World): Promise<Loose[]> => w.models["AdminLogEntry"].objects.using(w.db).orderBy(w.models["AdminLogEntry"].id);
const labels = (reply: AdminResponse): string[] => reply.json().items.map((i: Loose) => i.label);

function storageSite(w: World): Admin {
  return siteOf(w, { files: { cover: "media" } }, {}, { storage: new (loadRegistry())(new Map([["media", new Operator("memory")]])) });
}

function loadRegistry(): any {
  // The staged package of the extension; absent when the extension is not built.
  return (globalThis as Loose).__fileStorage;
}

if (files) {
  // A variable specifier keeps tsc out of the staged (generated) extension sources; `dist/test` sits two levels below `languages/js`.
  const specifier = "../../extensions/file-storage/index.js";
  (globalThis as Loose).__fileStorage = ((await import(specifier)) as Loose).Registry;
}

for (const provider of PROVIDERS) {
  test(`the autocomplete pages by keyset and searches (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w).testClient(w.db, { user: SU });
    const first = (await client.get(`${API}/autocomplete/RBook/author?limit=10`)).json();
    assert.deepEqual(first.items[0], { value: 1, label: "Ada" });
    assert.equal(first.items.length, 10);
    const seen: number[] = first.items.map((i: Loose) => i.value);
    let cursor: string | null = first.next;
    while (cursor) {
      const page = (await client.get(`${API}/autocomplete/RBook/author?limit=10&cursor=${cursor}`)).json();
      seen.push(...page.items.map((i: Loose) => i.value));
      cursor = page.next;
    }
    assert.deepEqual(seen, Array.from({ length: 30 }, (_, i) => i + 1), "every row once, in key order");
    assert.deepEqual(labels(await client.get(`${API}/autocomplete/RBook/author?q=Author%2007`)), ["Author 07"]);
    assert.equal((await client.get(`${API}/autocomplete/RBook/title`)).status, 404, "a column is not a relation field");
    assert.equal((await siteOf(w).testClient(w.db).get(`${API}/autocomplete/RBook/author`)).status, 401);
  }));

  test(`the autocomplete applies the queryset of the target (${provider})`, () => world(provider, async (w) => {
    const a = w.models["RAuthor"];
    const client = siteOf(w, {}, { queryset: a.objects.filter(a.active.eq(true)).prepare() }).testClient(w.db, { user: SU });
    assert.deepEqual(labels(await client.get(`${API}/autocomplete/RBook/author?limit=3`)), ["Ada", "Author 03", "Author 04"]);
  }));

  test(`a bad relation config fails the build (${provider})`, () => world(provider, async (w) => {
    const cases: [Loose, string][] = [
      [{ author: { label: "id" } }, "not a text column"],
      [{ nope: {} }, "not a relation"],
      [{ links: {} }, "not a resource"],
      [{ author: { colour: "red" } }, "unknown option"],
    ];
    for (const [relations, match] of cases) {
      assert.throws(() => siteOf(w, { relations }).check(), (e: unknown) => e instanceof AdminConfigError && e.message.includes(match), JSON.stringify(relations));
    }
  }));

  test(`a target with no search fails the build (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    const { RAuthor: a, RBook: b } = w.models;
    site.register(a, { list: { columns: [a.name] } } as never);
    site.register(b, { list: { columns: [b.title] }, relations: { author: {} } } as never);
    assert.throws(() => site.check(), (e: unknown) => e instanceof AdminConfigError && e.message.includes("has no search"));
  }));

  test(`choices limit the picker and a write outside them is a 422 (${provider})`, () => world(provider, async (w) => {
    const a = w.models["RAuthor"];
    const client = siteOf(w, { relations: { author: { choices: a.objects.filter(a.active.eq(true)).prepare() } } }).testClient(w.db, { user: SU });
    assert.deepEqual(labels(await client.get(`${API}/autocomplete/RBook/author?limit=3`)), ["Ada", "Author 03", "Author 04"]);
    const bad = await client.post(`${API}/resources/RBook`, { json: { set: { author_id: 2, title: "Bad" } } });
    assert.equal(bad.status, 422);
    assert.ok(bad.json().fields.author_id[0].includes("Choose"), bad.text);
    assert.equal((await client.post(`${API}/resources/RBook`, { json: { set: { author_id: 3, title: "Good" } } })).status, 201);
    const moved = await client.patch(`${API}/resources/RBook/2`, { json: { set: { author_id: 2 }, loaded: { author_id: 3 } } });
    assert.equal(moved.status, 422, "a changed value is checked on update");
    const same = await client.patch(`${API}/resources/RBook/2`, { json: { set: { title: "Renamed" }, loaded: { title: "Good" } } });
    assert.equal(same.status, 200, "an unchanged value stays");
  }));

  test(`the chips add and remove links in the save with one audit row (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w).testClient(w.db, { user: SU });
    assert.deepEqual(labels(await client.get(`${API}/links/RBook/1/tags`)).sort(), ["green", "red"]);
    const body = { set: { title: "Renamed" }, loaded: { title: "First" }, links: { tags: { add: [3, 4], remove: [1] } } };
    assert.equal((await client.patch(`${API}/resources/RBook/1`, { json: body })).status, 200);
    assert.deepEqual(await links(w, 1), [2, 3, 4]);
    const entries = await audit(w);
    assert.equal(entries.length, 1, "one audit row for the field and the links");
    assert.deepEqual(entries[0].changes.diff["links.tags"].removed, [1]);
    const only = await client.patch(`${API}/resources/RBook/1`, { json: { links: { tags: { add: [3], remove: [2] } } } });
    assert.equal(only.status, 200);
    assert.deepEqual(await links(w, 1), [3, 4]);
    const missing = await client.patch(`${API}/resources/RBook/1`, { json: { links: { tags: { add: [99] } } } });
    assert.equal(missing.status, 422);
    assert.deepEqual(await links(w, 1), [3, 4], "a failed save leaves the links");
  }));

  test(`a create takes chips in the same transaction (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w).testClient(w.db, { user: SU });
    const made = await client.post(`${API}/resources/RBook`, { json: { set: { author_id: 1, title: "New" }, links: { tags: { add: [1, 3] } } } });
    assert.equal(made.status, 201);
    assert.deepEqual(await links(w, 2), [1, 3]);
    const bad = await client.post(`${API}/resources/RBook`, { json: { set: { author_id: 1, title: "Bad" }, links: { tags: { add: [99] } } } });
    assert.equal(bad.status, 422);
    assert.equal(await w.models["RBook"].objects.using(w.db).count(), 2, "the book of the failed save is not stored");
    assert.equal((await audit(w)).length, 1);
  }));

  test(`the modal creates a related record without inlines (${provider})`, () => world(provider, async (w) => {
    const client = siteOf(w, { relations: { author: { create: true }, tags: {} } }).testClient(w.db, { user: SU });
    const meta = (await client.get(`${API}/resources`)).json();
    const form = Object.fromEntries(meta.resources.find((r: Loose) => r.name === "RBook").form.map((f: Loose) => [f.name, f]));
    assert.equal((form["author_id"] as Loose).relation.create, true);
    assert.equal((await client.post(`${API}/resources/RAuthor`, { json: { set: { name: "Cy" } } })).status, 201);
  }));

  test(`an upload stores the file and a download sends it as an attachment (${provider})`, { skip: !files }, () => world(provider, async (w) => {
    const client = storageSite(w).testClient(w.db, { user: SU });
    const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x20, 0x64, 0x61, 0x74, 0x61]);
    const up = await client.post(`${API}/files/RBook/1/cover`, { file: ["dir/cover 1.png", bytes, "image/png"] });
    assert.equal(up.status, 200, up.text);
    const value = up.json().value;
    assert.deepEqual([value.storage, value.filename, value.size], ["media", "cover 1.png", 9]);
    const book = w.models["RBook"];
    const stored = await book.objects.using(w.db).filter(book.id.eq(1)).first();
    assert.equal(stored.cover.key, value.key);
    assert.deepEqual((await audit(w)).map((e: Loose) => e.action), ["update"]);
    const down = await client.get(`${API}/files/RBook/1/cover`);
    assert.equal(down.status, 200);
    assert.deepEqual([...down.content], [...bytes]);
    const headers = Object.fromEntries(down.headers.map(([k, v]) => [k.toLowerCase(), v]));
    assert.equal(headers["content-type"], "image/png");
    assert.ok(headers["content-disposition"]!.startsWith("attachment;"));
    assert.ok(!Object.values(headers).join(" ").includes("http"), "no storage URL reaches the browser");
    assert.equal((await client.get(`${API}/files/RBook/1/title`)).status, 404, "a field that is not a file field");
    assert.equal((await client.post(`${API}/files/RBook/9/cover`, { file: ["a.png", "x", "image/png"] })).status, 404);
  }));

  test(`a plain set of a file field is a 422 and an upload over the cap is a 413 (${provider})`, { skip: !files }, () => world(provider, async (w) => {
    const client = storageSite(w).testClient(w.db, { user: SU });
    const plain = await client.patch(`${API}/resources/RBook/1`, { json: { set: { cover: { key: "x" } }, loaded: { cover: null } } });
    assert.equal(plain.status, 422);
    assert.ok(plain.json().fields.cover[0].includes("upload"));
    const big = await client.post(`${API}/files/RBook/1/cover`, { file: ["big.bin", new Uint8Array(26 * 1024 * 1024).fill(97), "application/octet-stream"] });
    assert.equal(big.status, 413);
  }));

  test(`an upload needs the csrf token and the origin (${provider})`, { skip: !files }, () => world(provider, async (w) => {
    const client = storageSite(w).testClient(w.db, { secret: SECRET });
    assert.equal((await client.login("ann", "pw")).status, 200);
    const path = `${API}/files/RBook/1/cover`;
    const file = ["a.txt", "text", "text/plain"] as const;
    assert.equal((await client.post(path, { file, csrf: null })).status, 403, "no CSRF token");
    assert.equal((await client.post(path, { file, origin: "http://evil.example" })).status, 403, "a foreign Origin");
    assert.equal((await client.post(path, { file })).status, 200);
    assert.equal(new TextDecoder().decode((await client.get(path)).content), "text");
  }));

  test(`a read only file field takes no upload (${provider})`, { skip: !files }, () => world(provider, async (w) => {
    const client = siteOf(w, { files: { cover: "media" }, readonly: ["cover"] }, {}, { storage: new (loadRegistry())(new Map([["media", new Operator("memory")]])) }).testClient(w.db, { user: SU });
    const up = await client.post(`${API}/files/RBook/1/cover`, { file: ["a.png", "x", "image/png"] });
    assert.equal(up.status, 422);
    assert.ok(up.json().fields.cover[0].includes("read-only"));
    const book = w.models["RBook"];
    assert.equal((await book.objects.using(w.db).filter(book.id.eq(1)).first()).cover, null);
    assert.deepEqual(await audit(w), []);
  }));
}
