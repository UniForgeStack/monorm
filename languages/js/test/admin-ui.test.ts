import assert from "node:assert/strict";
import { test } from "node:test";

import { Admin, AdminConfigError } from "../src/admin.js";
import { connect, loads, Registry } from "../src/index.js";

const SOURCE = `
datasource db {
  provider = "sqlite"
}

model UiAuthor {
  id   BigInt @id @default(autoincrement())
  name String
  @@map("ui_authors")
}

model UiMember {
  org  String
  slug String
  note String
  @@id([org, slug])
  @@map("ui_members")
}

model UiStamp {
  at DateTime @id
  @@map("ui_stamps")
}

model UiHidden {
  id BigInt @id @default(autoincrement())
  @@map("ui_hidden")
}
`;

const SU = { is_superuser: true };

// Models are loose here: the columns are reached by name.
type Loose = any;

async function world(run: (w: { registry: Registry; classes: Record<string, Loose>; db: Awaited<ReturnType<typeof connect>> }) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const classes = loads(SOURCE, { registry }) as Record<string, Loose>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await run({ registry, classes, db });
  } finally {
    await db.close();
  }
}

test("brand, groups, titles and nav reach the resources route", () => world(async ({ registry, classes, db }) => {
  const brand = { name: "Acme", colors: { primary: "#112233" }, links: [{ label: "Docs", url: "/docs" }], environment: "staging" };
  const site = new Admin(registry, { brand, groups: ["People", "Other"] });
  site.register(classes["UiAuthor"], { title: "Writers", group: "People" });
  site.register(classes["UiMember"], { group: "People" });
  site.register(classes["UiHidden"]);
  const body = (await site.testClient(db, { user: SU }).get("/admin/api/resources")).json();
  assert.equal(body.brand.name, "Acme");
  assert.equal(body.brand.environment, "staging");
  assert.deepEqual(body.brand.links, [{ label: "Docs", url: "/docs" }]);
  assert.deepEqual(body.nav, [
    { group: null, resources: ["UiHidden"] },
    { group: "People", resources: ["UiAuthor", "UiMember"] },
  ]);
  const titles = Object.fromEntries(body.resources.map((r: { name: string; title: string }) => [r.name, r.title]));
  assert.equal(titles["UiAuthor"], "Writers");
}));

test("an unknown brand key is refused", () => world(async ({ registry }) => {
  assert.throws(() => new Admin(registry, { brand: { nope: "x" } as Loose }), (e: Error) => e instanceof AdminConfigError && e.message.includes("nope"));
}));

test("a group that the admin does not list is refused", () => world(async ({ registry, classes }) => {
  const site = new Admin(registry, { groups: ["People"] });
  site.register(classes["UiAuthor"], { group: "Missing" });
  assert.throws(() => site.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes("Missing"));
}));

test("url takes a scalar, a list or an object", () => world(async ({ registry, classes }) => {
  const site = new Admin(registry);
  site.register(classes["UiAuthor"]);
  site.register(classes["UiMember"]);
  assert.equal(site.url(classes["UiAuthor"], 7), "/admin/UiAuthor/7");
  assert.equal(site.url(classes["UiAuthor"], 7n), "/admin/UiAuthor/7");
  assert.equal(site.url(classes["UiMember"], ["acme", "a b"]), "/admin/UiMember/acme,a%20b");
  assert.equal(site.url(classes["UiMember"], ["acme", "x,y"]), "/admin/UiMember/acme,x%2Cy");
  assert.equal(site.url(classes["UiMember"], { slug: "s", org: "o" }), "/admin/UiMember/o,s");
}));

test("url refuses a non-resource and a wrong key", () => world(async ({ registry, classes }) => {
  const site = new Admin(registry);
  site.register(classes["UiAuthor"]);
  site.register(classes["UiMember"]);
  assert.throws(() => site.url(classes["UiHidden"], 1), (e: Error) => e instanceof AdminConfigError && e.message.includes("not an admin resource"));
  assert.throws(() => site.url(classes["UiMember"], "only-one"), Error);
  assert.throws(() => site.url(classes["UiMember"], { org: "o", nope: "s" }), Error);
}));

test("url sends a datetime key as iso 8601 and refuses a value that the route refuses", () => world(async ({ registry, classes }) => {
  const site = new Admin(registry);
  site.register(classes["UiStamp"]);
  assert.equal(site.url(classes["UiStamp"], new Date("2026-01-02T03:04:05Z")), "/admin/UiStamp/2026-01-02T03%3A04%3A05.000Z");
  assert.throws(() => site.url(classes["UiStamp"], "2026-01-02 03:04:05"), Error);
}));

test("the url of a row opens in the detail route", () => world(async ({ registry, classes, db }) => {
  const site = new Admin(registry);
  site.register(classes["UiMember"]);
  await classes["UiMember"].objects.using(db).insertMany([{ org: "a/b", slug: "c,d", note: "hi" }]);
  const path = site.url(classes["UiMember"], ["a/b", "c,d"]).replace("/admin/", "/admin/api/resources/");
  const reply = await site.testClient(db, { user: SU }).get(path);
  assert.equal(reply.status, 200);
  const values = reply.json().values;
  assert.equal(values[values.length - 1], "hi");
}));
