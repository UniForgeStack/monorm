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

test("switcher, group icons, environment level and resource icon reach the resources route", () => world(async ({ registry, classes, db }) => {
  const switcher = {
    current: "shop",
    services: [
      { key: "shop", title: "Shop", icon: "store", color: "#0a7d5a" },
      { key: "ops", title: "Ops", url: "https://ops.example.com/admin", icon: "wrench" },
    ],
    build: { commit: "0123456789abcdef", builtAt: "2026-11-24T10:00:00Z", tree: "fedcba9876543210" },
  };
  const site = new Admin(registry, { brand: { environment: "Production" }, groups: ["People"], groupIcons: { People: "users" }, switcher });
  site.register(classes["UiAuthor"], { group: "People", icon: "pen-line" });
  const body = (await site.testClient(db, { user: SU }).get("/admin/api/resources")).json();
  const brand = body.brand;
  assert.equal(brand.environment_level, "danger");
  assert.equal(brand.colors.primary, "#0a7d5a");
  assert.equal(brand.switcher.current, "shop");
  assert.deepEqual(brand.switcher.services.map((s: { key: string }) => s.key), ["shop", "ops"]);
  assert.equal(brand.switcher.services[1].url, "https://ops.example.com/admin");
  assert.equal(brand.switcher.build.commit, "0123456789abcdef");
  assert.equal(body.nav[0].icon, "users");
  assert.equal(body.resources.find((r: { name: string }) => r.name === "UiAuthor").icon, "pen-line");
}));

test("a wrong switcher is refused", () => world(async ({ registry, classes }) => {
  const site = new Admin(registry, { switcher: { current: "missing", services: [{ key: "shop", title: "Shop", url: "/shop" }] } });
  site.register(classes["UiAuthor"]);
  assert.throws(() => site.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes("switcher.current"));
  assert.throws(() => new Admin(registry, { switcher: { nope: 1 } as Loose }), (e: Error) => e instanceof AdminConfigError && e.message.includes("nope"));
  const shop = { key: "shop", title: "Shop", url: "/shop" };
  const cases: [Loose, string][] = [
    [{ brand: { environmentLevel: "info" } }, "needs brand.environment"],
    [{ groupIcons: { Sales: "users" } }, "groupIcons"],
    [{ switcher: { current: "shop", services: [{ ...shop, icon: "Shield!" }] } }, "icon"],
    [{ switcher: { current: "shop", services: [shop, { key: "ops", title: "Ops", url: "http://ops.example.com" }] } }, "https"],
    [{ brand: { colors: { primary: "#111111" } }, switcher: { current: "shop", services: [{ ...shop, color: "#222222" }] } }, "primary"],
  ];
  for (const [options, match] of cases) {
    const bad = new Admin(registry, options);
    bad.register(classes["UiAuthor"]);
    assert.throws(() => bad.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes(match));
  }
}));

test("a theme is the short form of the brand and a value in both is refused", () => world(async ({ registry, classes, db }) => {
  const site = new Admin(registry, { theme: { title: "Acme", logo: "/static/acme.svg", color: "#0a7d5a" } });
  site.register(classes["UiAuthor"]);
  const brand = (await site.testClient(db, { user: SU }).get("/admin/api/resources")).json().brand;
  assert.equal(brand.name, "Acme");
  assert.equal(brand.logo, "/static/acme.svg");
  assert.equal(brand.colors.primary, "#0a7d5a");
  const clash = new Admin(registry, { brand: { name: "Other" }, theme: { title: "Acme" } });
  clash.register(classes["UiAuthor"]);
  assert.throws(() => clash.check(), (e: Error) => e instanceof AdminConfigError && e.message.includes("theme.title"));
  assert.throws(() => new Admin(registry, { theme: { shade: "x" } as Loose }), (e: Error) => e instanceof AdminConfigError && e.message.includes("shade"));
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
