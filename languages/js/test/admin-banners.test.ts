// The `banners` resource option: one host call per detail read, the banners in the detail body, one danger banner on a failure.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { connect, load, Registry, type Database } from "../src/index.js";

const API = "/admin/api/resources/Note";
const ROOT = { key: 1, is_superuser: true };
const SCHEMA = `datasource db {
  provider = "sqlite"
}
import "app:auth" (prefix: "auth_")
import "app:admin" (prefix: "admin_")

model Note {
  id    Int @id
  title String
}
`;

type Loose = any;

async function withWorld(run: (clientOf: (banners: unknown) => Loose, siteOf: (banners: unknown) => Admin) => Promise<void>) {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-banners-"));
  let db: Database | undefined;
  try {
    writeFileSync(join(dir, "package.json"), "{}");
    writeFileSync(join(dir, "schema.prisma"), SCHEMA);
    const registry = new Registry();
    const models = load(join(dir, "schema.prisma"), { registry }) as unknown as Record<string, Loose>;
    db = await connect("sqlite://:memory:", { registry, default: false });
    await db.createTables();
    await models["Note"].objects.using(db).insertMany([{ id: 1, title: "Alpha" }, { id: 2, title: "Beta" }]);
    const database = db;
    const siteOf = (banners: unknown) => {
      const site = new Admin(registry);
      site.register(models["Note"], { banners, list: { columns: ["title"], order: ["id"] } } as Loose);
      return site;
    };
    await run((banners) => siteOf(banners).testClient(database, { user: ROOT }), siteOf);
  } finally {
    if (db) await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a banners function gets the record once per detail read", () => withWorld(async (clientOf) => {
  const seen: Loose[] = [];
  const client = clientOf(async (record: Loose) => {
    seen.push(record);
    return [{ level: "warning", text: `Check ${record.title}` }];
  });
  const body = (await client.get(`${API}/2`)).json();
  assert.deepEqual(body.banners, [{ level: "warning", text: "Check Beta" }]);
  assert.deepEqual(seen.map((r) => r.title), ["Beta"]);
  assert.equal((await client.get(API)).status, 200);
  assert.equal(seen.length, 1, "a list read does not call the function");
}));

test("a failing or wrong banners function gives one danger banner and the page loads", () => withWorld(async (clientOf) => {
  const fails = async () => { throw new Error("secret detail"); };
  const wrong = async () => [{ level: "loud", text: "x" }];
  for (const fn of [fails, wrong]) {
    const response = await clientOf(fn).get(`${API}/1`);
    assert.equal(response.status, 200);
    const banners = response.json().banners;
    assert.deepEqual(banners.map((b: Loose) => b.level), ["danger"]);
    assert.ok(!banners[0].text.includes("secret"));
  }
}));

test("banners over a limit are cut and no danger banner shows", () => withWorld(async (clientOf) => {
  const many = async () => Array.from({ length: 25 }, (_, i) => ({ level: "info", text: `n${i}` }));
  const long = async () => [{ level: "warning", text: "é".repeat(600) }];
  let banners = (await clientOf(many).get(`${API}/1`)).json().banners;
  assert.deepEqual(banners.map((b: Loose) => b.text), Array.from({ length: 20 }, (_, i) => `n${i}`));
  assert.ok(banners.every((b: Loose) => b.level === "info"));
  banners = (await clientOf(long).get(`${API}/1`)).json().banners;
  assert.deepEqual(banners.map((b: Loose) => b.level), ["warning"]);
  assert.equal(banners[0].text.length, 500);
}));

test("a banners function that is not callable is refused", () => withWorld(async (_clientOf, siteOf) => {
  assert.throws(() => siteOf("warn").check(), (e) => e instanceof AdminConfigError && e.message.includes("not a host function"));
}));
