// Frame and embed configuration and requests on SQLite and PostgreSQL.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, type AdminOptions, type EmbedConfig, type FrameConfig, type ResourceConfig } from "../src/admin.js";
import { connect, load, Registry, type Database } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const BASE = "/back/admin";
const API = `${BASE}/api`;
const ITEMS = `${API}/resources/FrameItem`;
const EMBED = `${API}/embeds/health`;
const SPECIAL = "é /?#%&=+";
const schema = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:auth" (prefix: "auth_")
import "app:admin" (prefix: "admin_")

model FrameOwner {
  id    Int @id
  name  String
  items FrameItem[]
}

model FrameItem {
  id         Int @id
  domain     String @unique
  title      String
  secret     String?
  payload    Json?
  owner_id   Int?
  owner      FrameOwner? @relation(fields: [owner_id], references: [id])
}
`;

type Loose = any;
interface World {
  registry: Registry;
  models: Record<string, Loose>;
  db: Database;
  dir: string;
}

async function withWorld(provider: string, run: (world: World) => Promise<void>) {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-frames-"));
  let db: Database | undefined;
  try {
    writeFileSync(join(dir, "package.json"), "{}");
    const path = join(dir, "schema.prisma");
    writeFileSync(path, schema(provider));
    const registry = new Registry();
    const models = load(path, { registry }) as unknown as Record<string, Loose>;
    try {
      db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
    } catch (error) {
      if (process.env["ORM_TEST_DATABASE_URL"] !== undefined || provider === "sqlite") throw error;
      return;
    }
    await db.dropTables();
    await db.createTables();
    await models["User"].objects.using(db).insertMany([
      { username: "reader", email: "reader@example.com", password: "unused" },
      { username: "denied", email: "denied@example.com", password: "unused" },
      { username: "hopper", email: "hopper@example.com", password: "unused" },
      { username: "linked", email: "linked@example.com", password: "unused" },
    ]);
    await models["FrameOwner"].objects.using(db).insertMany([{ id: 1, name: "Private owner" }]);
    await models["FrameItem"].objects.using(db).insertMany([
      { id: 1, domain: SPECIAL, title: "Healthy", secret: "Internal", ownerId: 1 },
      { id: 2, domain: "nullable", title: "Empty" },
      { id: 3, domain: "outside", title: "Out of scope" },
    ]);
    await run({ registry, models, db, dir });
  } finally {
    if (db) {
      await db.dropTables();
      await db.close();
    }
    rmSync(dir, { recursive: true, force: true });
  }
}

function frame(url: string, extra: Partial<FrameConfig> = {}): FrameConfig {
  return { name: "health", title: "Health frame", url, ...extra };
}

function embed(extra: Partial<EmbedConfig> = {}): EmbedConfig {
  return { resource: "shops.FrameItem", lookup: "domain", tab: "health_info", ancestors: ["'self'"], ...extra };
}

function build({ registry, models }: World, options: AdminOptions = {}, resource: ResourceConfig = {}) {
  const site = new Admin(registry, { basePath: BASE, ...options });
  site.register(models["FrameItem"], {
    see: { secret: "see_secret" },
    fieldsets: [{ name: "health_info", title: "Health", fields: ["title", "secret"] }],
    ...resource,
  });
  site.register(models["FrameOwner"], { see: { name: "see_owner_name" } });
  return site;
}

async function grant({ db, models }: World, username: string, names: string[]) {
  const rows: Loose[] = (await models["Permission"].objects.using(db)).filter((p: Loose) => names.includes(`${p.label}.${p.codename}`));
  assert.equal(rows.length, names.length);
  const Role = models["Role"];
  await Role.objects.using(db).insertMany([{ name: `for-${username}` }]);
  const role = await Role.objects.using(db).filter(Role.name.eq(`for-${username}`)).first();
  const User = models["User"];
  const user = await User.objects.using(db).filter(User.username.eq(username)).first();
  await models["RolePermission"].objects.using(db).insertMany(rows.map((p) => ({ roleId: role.id, permissionId: p.id })));
  await models["UserRole"].objects.using(db).insertMany([{ userId: user.id, roleId: role.id }]);
  return user.id;
}

function configError(site: Admin, message: string) {
  assert.throws(() => site.check(), (error) => error instanceof AdminConfigError && error.message.includes(message), message);
}

for (const provider of ["sqlite", "postgres"]) {
  test(`frame config errors (${provider})`, () => withWorld(provider, (w) => {
    for (const origin of ["https://example.com/path", "https://example.com/", "https://*.example.com", "http://example.com", "https://example.com;"]) {
      configError(build(w, { frameOrigins: [origin] }), "invalid frame origin");
    }
    for (const [url, problem] of [
      ["//evil.example/path", "invalid frame URL"],
      ["https://other.example/path", "frame_origins"],
      ["https://{domain}.example/path", "invalid frame URL"],
      ["/identity?{domain}=x", "invalid frame URL"],
      ["/identity?x=prefix{domain}", "invalid frame URL"],
      ["/identity#{domain}", "invalid frame URL"],
      ["/identity/{payload}", "must be scalar"],
      ["/identity/{misspelled}", "unknown field"],
      ["https://example.com:{domain}/identity", "invalid frame URL"],
      ["/outside/../back/admin/FrameItem", "points into this admin"],
    ] as const) {
      configError(build(w, { frameOrigins: ["'self'"] }, { frames: [frame(url)] }), problem);
    }
    configError(build(w, {}, { frames: [frame("/identity")] }), "frame_origins");
    configError(build(w, { frameOrigins: ["'self'"] }, { frames: [frame("/identity", { sandbox: ["allow-top-navigation"] })] }), "sandbox flag");
    configError(build(w, { frameOrigins: ["'self'"], frameCards: [frame("/identity/{domain}")] }), "cannot have placeholders");
    const origins = { frameOrigins: ["'self'"] };
    for (const [resource, problem] of [
      [{ frames: [frame("/identity", { name: "Invalid" })] }, "frame name"],
      [{ frames: [frame("/identity", { title: "" })] }, "unique name and a title"],
      [{ frames: [frame("/identity", { name: "health_info" })] }, "duplicate detail tab"],
      [{ fieldsets: [{ name: "Invalid", fields: ["title"] }] }, "fieldset name"],
      [{ fieldsets: [{ name: "empty", fields: [] }] }, "at least one field"],
      [{ fieldsets: [{ name: "missing", fields: ["missing"] }] }, "unknown or repeated field"],
    ] as const) {
      configError(build(w, origins, resource), problem);
    }
    configError(build(w, { embeds: { Invalid: embed() } }), "embed name");
    for (const [config, problem] of [
      [embed({ lookup: "title" }), "unique scalar field"],
      [embed({ tab: "missing" }), "unknown detail tab"],
      [embed({ resource: "Missing" }), "unknown resource"],
      [embed({ ancestors: [] }), "at least one origin"],
      [embed({ ancestors: ["https://parent.example/path"] }), "invalid frame origin"],
    ] as const) {
      configError(build(w, { embeds: { health: config } }), problem);
    }
    writeFileSync(join(w.dir, "summary.js"), "export const adminMajor = 1;");
    const site = build(w, { embeds: { health: embed({ tab: "reports.summary" }) } }, { detailTabs: ["reports.summary"] });
    site.package("reports", w.dir, { tabs: { summary: "summary.js" } });
    configError(site, "embeddable: true");
    return Promise.resolve();
  }));

  test(`frame headers and home cards (${provider})`, () => withWorld(provider, async (w) => {
    const site = build(w, {
      frameOrigins: ["'self'", "https://charts.example:8443", "http://localhost:1234"],
      frameCards: [frame("https://charts.example:8443/health", { scripts: false, sameOrigin: false })],
      embeds: { health: embed(), external: embed({ ancestors: ["'self'", "https://portal.example"] }) },
    });
    const browser = site.testClient(w.db, { secret: "a".repeat(40) });
    for (const [path, status, ancestors, xfo] of [
      [BASE, 200, "'none'", "DENY"],
      [`${BASE}/FrameItem/1`, 200, "'none'", "DENY"],
      [`${BASE}/-/embed/health`, 200, "'self'", "SAMEORIGIN"],
      [`${BASE}/-/embed/external`, 200, "'self' https://portal.example", undefined],
      [`${BASE}/-/embed/missing`, 404, "'none'", "DENY"],
      [`${BASE}/-/embed/health/extra`, 404, "'none'", "DENY"],
      [EMBED, 401, "'none'", "DENY"],
    ] as const) {
      const response = await browser.get(path);
      const headers = new Map(response.headers);
      const csp = headers.get("content-security-policy")!;
      assert.equal(response.status, status, path);
      assert.ok(csp.split("; ").includes(`frame-ancestors ${ancestors}`), csp);
      assert.equal(headers.get("x-frame-options"), xfo);
      if (status === 200) assert.ok(csp.includes("frame-src 'self' https://charts.example:8443 http://localhost:1234"));
      else assert.ok(!csp.includes("frame-src"));
    }
    const noFrames = await build(w).testClient(w.db, { secret: "b".repeat(40) }).get(BASE);
    assert.ok(!new Map(noFrames.headers).get("content-security-policy")!.includes("frame-src"));
    const home = await site.testClient(w.db, { user: { is_superuser: true } }).get(`${API}/site`);
    assert.deepEqual(home.json().frame_cards, [{
      name: "health", title: "Health frame", url: "https://charts.example:8443/health",
      sandbox: "", height: "70vh", referrer_policy: "no-referrer",
    }]);
  }));

  test(`frame detail and embed obey permissions and scope (${provider})`, () => withWorld(provider, async (w) => {
    const item = w.models["FrameItem"];
    const site = build(w, { frameOrigins: ["'self'"], embeds: { health: embed(), package: embed({ tab: "reports.summary" }) } }, {
      queryset: item.objects.filter(item.id.lt(3)).prepare(),
      frames: [
        frame("/identity/{domain}?domain={domain}", { sandbox: ["allow-forms"] }),
        frame("/identity?token={secret}", { name: "secret" }),
        frame("/identity?owner={owner.name}", { name: "owner" }),
      ],
      detailTabs: ["reports.summary"],
    });
    writeFileSync(join(w.dir, "summary.js"), "export const adminMajor = 1;");
    site.package("reports", w.dir, { tabs: { summary: { module: "summary.js", embeddable: true } } });
    await site.syncPermissions(w.db);
    const reader = site.testClient(w.db, { user: { key: await grant(w, "reader", ["admin.access", "admin.manage_frameitem"]) } });
    const denied = site.testClient(w.db, { user: { key: await grant(w, "denied", ["admin.access"]) } });
    const root = site.testClient(w.db, { user: { is_superuser: true } });
    const rootDetail = await root.get(`${ITEMS}/1`);
    assert.equal(rootDetail.status, 200, rootDetail.text);
    const frames = rootDetail.json().frames;
    const url = new URL(frames[0].url, "https://site.example");
    assert.equal(url.pathname, "/identity/%C3%A9%20%2F%3F%23%25%26%3D%2B");
    assert.equal(url.searchParams.get("domain"), SPECIAL);
    assert.equal(frames[0].sandbox, "allow-scripts allow-same-origin allow-forms");
    assert.equal(frames[0].referrer_policy, "same-origin");
    assert.equal(frames[1].url, "/identity?token=Internal");
    assert.equal(frames[2].url, "/identity?owner=Private+owner");
    assert.equal((await root.get(`${ITEMS}/2`)).json().frames[1].url, null);

    const detail = await reader.get(`${ITEMS}/1`);
    assert.deepEqual(detail.json().frames.map((f: Loose) => f.url), [frames[0].url, null, null]);
    assert.ok(!detail.json().fields.some((f: Loose) => f.label === "owner.name"));
    const query = new URLSearchParams({ domain: SPECIAL });
    const response = await reader.get(`${EMBED}?${query}`);
    assert.equal(response.status, 200, response.text);
    assert.equal(response.queries, detail.queries);
    const body = response.json();
    assert.deepEqual(Object.keys(body).sort(), ["key", "tab", "title"]);
    assert.deepEqual([body.title, body.key], ["Health", "1"]);
    assert.deepEqual(body.tab.fields.map((f: Loose) => f.label), ["title"]);
    assert.deepEqual(body.tab.values, ["Healthy"]);
    assert.ok(!(await reader.get(`${API}/embeds/package?${query}`)).text.includes("Internal"));
    const pkg = (await root.get(`${API}/embeds/package?${query}`)).json().tab;
    assert.deepEqual(pkg.package.tabs, { summary: "packages/reports/summary.js" });
    const outside = await reader.get(`${EMBED}?domain=outside&without=scope`);
    const missing = await reader.get(`${EMBED}?domain=missing`);
    assert.deepEqual([outside.status, missing.status], [404, 404]);
    assert.deepEqual(outside.json(), missing.json());
    assert.equal((await denied.get(`${EMBED}?${query}`)).status, 403);
    assert.equal((await reader.get(`${EMBED}?domain=one&domain=two`)).status, 400);
    assert.equal(Number(await w.models["AdminLogEntry"].objects.using(w.db).count()), 0);
    assert.equal(Number(await item.objects.using(w.db).count()), 3);
  }));
}

for (const provider of ["sqlite", "postgres"]) {
  test(`frame relation hop needs the read grant of the target (${provider})`, () => withWorld(provider, async (w) => {
    const site = build(w, { frameOrigins: ["'self'"] }, { frames: [frame("/identity?owner={owner.name}", { name: "owner" })] });
    await site.syncPermissions(w.db);
    const see = ["admin.access", "admin.manage_frameitem", "admin.see_owner_name"];
    const hopper = site.testClient(w.db, { user: { key: await grant(w, "hopper", see) } });
    const linked = site.testClient(w.db, { user: { key: await grant(w, "linked", [...see, "admin.manage_frameowner"]) } });
    assert.equal((await hopper.get(`${ITEMS}/1`)).json().frames[0].url, null);
    assert.equal((await linked.get(`${ITEMS}/1`)).json().frames[0].url, "/identity?owner=Private+owner");
  }));
}
