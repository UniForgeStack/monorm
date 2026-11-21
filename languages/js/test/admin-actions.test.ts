// Admin actions: the placements, the query set, appliesTo, the second confirmation, bulk delete, the results and the grouped audit,
// through the bridge on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { action, Admin, AdminConfigError, Context, download, input, message, redirect, type ResourceConfig } from "../src/admin.js";
import { allowWrites, connect, load, Registry, type Database } from "../src/index.js";
import { DoesNotExist, ValidationError, VersionConflict } from "../src/errors.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model AShop {
  id    Int     @id @default(autoincrement())
  name  String  @db.VarChar(20)
  items AItem[]
  @@map("a_shops")
}

model AItem {
  id      Int      @id @default(autoincrement())
  shop_id Int
  name    String   @db.VarChar(20)
  status  String   @default("new") @db.VarChar(10)
  qty     Int      @default(0)
  created DateTime @default(now())
  shop    AShop    @relation(fields: [shop_id], references: [id], onDelete: Cascade)
  @@map("a_items")
}

model ABig {
  id  BigInt @id
  tag String @db.VarChar(10)
  @@map("a_bigs")
}

model AVault {
  id   Int    @id @default(autoincrement())
  note String @db.VarChar(10)
  @@write_rules.protect(layer: app)
  @@map("a_vaults")
}
`;

const ITEMS = "/admin/api/actions/AItem";
const SU = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-actions-"));
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
    await models["AShop"].objects.using(db).insertMany([{ name: "one" }, { name: "two" }]);
    await models["AItem"].objects.using(db).insertMany([
      { shopId: 1, name: "a", qty: 1 },
      { shopId: 1, name: "b", qty: 2 },
      { shopId: 1, name: "c", status: "locked", qty: 3 },
      { shopId: 2, name: "d", qty: 4 },
      { shopId: 2, name: "e", status: "done", qty: 5 },
    ]);
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const names = async (w: World, ...where: Loose[]): Promise<string[]> => {
  const item = w.models["AItem"];
  const qs = where.length > 0 ? item.objects.using(w.db).filter(...where) : item.objects.using(w.db);
  return (await qs.orderBy(item.id)).map((r: Loose) => r.name);
};

const qtys = async (w: World): Promise<[string, number][]> => {
  const item = w.models["AItem"];
  return (await item.objects.using(w.db).orderBy(item.id)).map((r: Loose) => [r.name, r.qty]);
};

const audit = async (w: World): Promise<[string, string, string, Loose][]> => {
  const entry = w.models["AdminLogEntry"];
  return (await entry.objects.using(w.db).orderBy(entry.id)).map((r: Loose) => [r.action, r.resource, r.recordKey, r.changes]);
};

const sorted = async (qs: Loose): Promise<string[]> => (await qs).map((r: Loose) => r.name).sort();

function siteOf(w: World, actions: ResourceConfig["actions"], item: Loose = {}): Admin {
  const site = new Admin(w.registry);
  const Item = w.models["AItem"];
  site.register(w.models["AShop"]);
  site.register(Item, { list: { columns: ["name"], filters: ["status", "created"] }, actions, ...item } as ResourceConfig);
  return site;
}

const post = (client: Loose, name: string, body: unknown, path = ITEMS) => client.post(`${path}/${name}`, { json: body });

for (const provider of PROVIDERS) {
  // -- the query set and the placements ---------------------------------------------------------------------------

  test(`a bulk action gets the selected rows with the can rule and the result is a message (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const seen: unknown[] = [];
    const double = action(
      async function double(ctx, qs) {
        seen.push([ctx.count, ctx.capped, ctx.selection, await sorted(qs)]);
        await qs.update({ qty: item.qty.mul(2) });
        return message(`Doubled ${ctx.count}`, "success");
      },
      { placement: "bulk", confirm: "Double these?" },
    );
    const site = siteOf(w, [double], { can: { change: item.status.ne("locked") } });
    const client = site.testClient(w.db, { user: SU });
    const r = await post(client, "double", { keys: ["1", "3"], inputs: {} });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(r.json().result, { kind: "message", message: "Doubled 1", level: "success" });
    assert.deepEqual(seen, [[1, false, "keys", ["a"]]]);
    assert.deepEqual(await qtys(w), [["a", 2], ["b", 2], ["c", 3], ["d", 4], ["e", 5]]);
    const rows = await audit(w);
    assert.deepEqual(rows.map(([a, res, key]) => [a, res, key]), [["action:double", "AItem", ""]]);
    const summary = rows[0]![3];
    assert.deepEqual([summary.placement, summary.count, summary.selection, summary.keys], ["bulk", 1, "keys", ["1", "3"]]);
    const meta = (await client.get("/admin/api/resources")).json();
    const actions = meta.resources.find((x: Loose) => x.name === "AItem").actions;
    assert.deepEqual(actions, [{ name: "double", label: "Double", placement: "bulk", confirm: "Double these?", inputs: [], applies: false }]);
  }));

  test(`all rows that match the list filters run in batches with a date filter and a search free query set (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    await item.objects.using(w.db).insertMany(Array.from({ length: 1200 }, (_, i) => ({ shopId: 1, name: `m${i}`, qty: 0 })));
    const seen: unknown[] = [];
    const bump = action(
      async function bump(ctx, qs) {
        let batches = 0;
        for await (const batch of qs.batches(500)) {
          batches += 1;
          await item.objects.using(ctx.database).filter(item.id.in(batch.map((r: Loose) => r.id))).update({ qty: 1 });
        }
        seen.push([ctx.count, ctx.selection, batches]);
      },
      { placement: "bulk" },
    );
    const client = siteOf(w, [bump]).testClient(w.db, { user: SU });
    const r = await client.post(`${ITEMS}/bump?f.status=new&f.created:gte=2020-01-01`, { json: { all: true, inputs: {} } });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [[1203, "all", 3]]);
    assert.equal(await item.objects.using(w.db).filter(item.qty.eq(1)).count(), 1203);
    assert.equal(await item.objects.using(w.db).filter(item.status.ne("new")).count(), 2);
  }));

  test(`an action on all rows with no filter may update them with qs update (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const zero = action(
      async function zero(_ctx, qs) {
        await qs.update({ qty: 0 });
      },
      { placement: "bulk" },
    );
    const client = siteOf(w, [zero]).testClient(w.db, { user: SU });
    const r = await client.post(`${ITEMS}/zero`, { json: { all: true, inputs: {} } });
    assert.equal(r.status, 200, r.text);
    assert.equal(await item.objects.using(w.db).filter(item.qty.eq(0)).count(), 5);
  }));

  test(`row detail submit and list placements each run on the rows they name (${provider})`, () => world(provider, async (w) => {
    const seen: unknown[] = [];
    const make = (placement: "row" | "detail" | "submit" | "list") =>
      action(
        async function run(ctx, qs) {
          seen.push([placement, ctx.selection, ctx.count, await sorted(qs)]);
        },
        { name: placement, placement },
      );
    const client = siteOf(w, (["row", "detail", "submit", "list"] as const).map(make)).testClient(w.db, { user: SU });
    for (const placement of ["row", "detail", "submit"]) {
      const r = await post(client, placement, { keys: ["2"], inputs: {} });
      assert.ok(r.status === 200 && JSON.stringify(r.json().result) === JSON.stringify({ kind: "refresh" }), r.text);
    }
    assert.equal((await post(client, "row", { keys: ["1", "2"], inputs: {} })).status, 400);
    assert.equal((await post(client, "list", { keys: ["1"], inputs: {} })).status, 400);
    const r = await client.post(`${ITEMS}/list?f.status=new`, { json: { inputs: {} } });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [
      ["row", "keys", 1, ["b"]],
      ["detail", "keys", 1, ["b"]],
      ["submit", "keys", 1, ["b"]],
      ["list", "filter", 3, ["a", "b", "d"]],
    ]);
  }));

  test(`appliesTo flags the rows and the query set leaves out the rows it does not cover (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const seen: unknown[] = [];
    const ship = action(
      async function ship(_ctx, qs) {
        seen.push(await sorted(qs));
      },
      { placement: "row", appliesTo: item.status.eq("new") },
    );
    const client = siteOf(w, [ship]).testClient(w.db, { user: SU });
    const page = (await client.get("/admin/api/resources/AItem")).json();
    assert.ok(page.rows.length > 0 && JSON.stringify(page).includes("applies:ship"), JSON.stringify(page));
    assert.equal((await post(client, "ship", { keys: ["1"], inputs: {} })).status, 200);
    assert.equal((await post(client, "ship", { keys: ["3"], inputs: {} })).status, 404);
    assert.deepEqual(seen, [["a"]]);
  }));

  test(`above ten thousand rows the action needs a second confirmation (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    await item.objects.using(w.db).insertMany(Array.from({ length: 10_000 }, () => ({ shopId: 1, name: "x" })));
    const seen: unknown[] = [];
    const touch = action(
      async function touch(ctx) {
        seen.push([ctx.count, ctx.capped]);
      },
      { placement: "bulk" },
    );
    const client = siteOf(w, [touch]).testClient(w.db, { user: SU });
    let r = await post(client, "touch", { all: true, inputs: {} });
    assert.ok(r.status === 409 && r.json().error.kind === "ConfirmLarge" && r.json().capped === true, r.text);
    assert.deepEqual(seen, []);
    r = await post(client, "touch", { all: true, inputs: {}, confirm_large: true });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [[10_000, true]]);
  }));

  // -- inputs and results -----------------------------------------------------------------------------------------

  test(`inputs are checked and a file input arrives as a file and is masked in the audit (${provider})`, () => world(provider, async (w) => {
    const seen: unknown[] = [];
    const loadSheet = action(
      async function load(ctx) {
        const sheet = ctx.inputs["sheet"] as Loose;
        seen.push([ctx.inputs["reason"], sheet.filename, sheet.content_type, readFileSync(sheet.path).toString()]);
      },
      { placement: "bulk", inputs: [input("reason", { required: true }), input("token", { secret: true }), input("sheet", { type: "file" })] },
    );
    const client = siteOf(w, [loadSheet]).testClient(w.db, { user: SU });
    assert.equal((await post(client, "load", { keys: ["1"], inputs: { token: "t" } })).status, 422);
    const sheet = { name: "rows.csv", content_type: "text/csv", base64: Buffer.from("a,b\n").toString("base64") };
    const r = await post(client, "load", { keys: ["1"], inputs: { reason: "why", token: "s3cret", sheet } });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [["why", "rows.csv", "text/csv", "a,b\n"]]);
    const inputs = (await audit(w))[0]![3].inputs;
    assert.equal(inputs.token, "***");
    assert.deepEqual(inputs.sheet, { name: "rows.csv", content_type: "text/csv", size: 4 });
  }));

  test(`a result is a message a redirect a download or a refresh and a bad one is a 500 (${provider})`, () => world(provider, async (w) => {
    const show = action(
      async function show(ctx) {
        const results: Record<string, unknown> = {
          message: message("done", "warning"),
          redirect: redirect("/resources/AShop"),
          download: download("out.txt", "hello", "text/plain"),
          text: "plain text",
          bad: 42,
        };
        return results[ctx.inputs["kind"] as string] as Loose;
      },
      { placement: "detail", inputs: [input("kind")] },
    );
    const client = siteOf(w, [show]).testClient(w.db, { user: SU });
    const run = (kind: string | null) => post(client, "show", { keys: ["1"], inputs: kind ? { kind } : {} });
    assert.deepEqual((await run("message")).json().result, { kind: "message", message: "done", level: "warning" });
    assert.deepEqual((await run("redirect")).json().result, { kind: "redirect", to: "/resources/AShop" });
    assert.deepEqual((await run("download")).json().result, { kind: "download", filename: "out.txt", content_type: "text/plain", base64: Buffer.from("hello").toString("base64") });
    assert.deepEqual((await run("text")).json().result, { kind: "message", message: "plain text", level: "info" });
    assert.deepEqual((await run(null)).json().result, { kind: "refresh" });
    assert.equal((await run("bad")).status, 500);
  }));

  test(`validation errors and missing rows of an action map to the status of a write (${provider})`, () => world(provider, async (w) => {
    const fail = action(
      async function fail(ctx) {
        if (ctx.inputs["mode"] === "invalid") throw new ValidationError({ qty: ["too big"] }, { nonFieldMessages: ["not now"] });
        if (ctx.inputs["mode"] === "missing") throw new DoesNotExist("gone");
        throw new VersionConflict("stale");
      },
      { placement: "row", inputs: [input("mode")] },
    );
    const client = siteOf(w, [fail]).testClient(w.db, { user: SU });
    const r = await post(client, "fail", { keys: ["1"], inputs: { mode: "invalid" } });
    assert.ok(r.status === 422 && JSON.stringify(r.json().fields) === JSON.stringify({ qty: ["too big"] }), r.text);
    assert.equal((await post(client, "fail", { keys: ["1"], inputs: { mode: "missing" } })).status, 404);
    assert.equal((await post(client, "fail", { keys: ["1"], inputs: {} })).status, 409);
    assert.deepEqual(await audit(w), []);
  }));

  test(`an action audit row and its writes roll back together and commit together (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const bump = action(
      async function bump(ctx, qs) {
        await qs.update({ qty: 99 });
        for (const row of await qs) await ctx.audit(row, { action: "bumped", changes: { qty: 99 } });
        if (ctx.inputs["fail"]) throw new Error("the action fails after its audit rows");
      },
      { placement: "bulk", inputs: [input("fail", { type: "checkbox" })] },
    );
    const client = siteOf(w, [bump]).testClient(w.db, { user: SU });
    const before = await qtys(w);
    const r = await post(client, "bump", { keys: ["1", "2"], inputs: { fail: true } });
    assert.ok(r.status >= 500, r.text);
    assert.deepEqual(await qtys(w), before);
    assert.deepEqual(await audit(w), []);
    const ok = await post(client, "bump", { keys: ["1", "2"], inputs: {} });
    assert.equal(ok.status, 200, ok.text);
    assert.deepEqual((await qtys(w)).slice(0, 2), [["a", 99], ["b", 99]]);
    assert.deepEqual((await audit(w)).map(([a]) => a), ["bumped", "bumped", "action:bump"]);
  }));

  // -- bulk delete ------------------------------------------------------------------------------------------------

  test(`bulk delete is one delete of the query set and bulkDelete false removes it (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const client = siteOf(w, [], { can: { delete: item.status.ne("locked") } }).testClient(w.db, { user: SU });
    let r = await post(client, "bulk_delete", { keys: ["1", "3", "4"], inputs: {} });
    assert.ok(r.status === 200 && r.json().affected === 2, r.text);
    assert.deepEqual(await names(w), ["b", "c", "e"]);
    r = await client.post(`${ITEMS}/bulk_delete?f.status=done`, { json: { all: true, inputs: {} } });
    assert.ok(r.status === 200 && r.json().affected === 1, r.text);
    assert.deepEqual((await audit(w)).map(([a, , key]) => [a, key]), [["bulk_delete", ""], ["bulk_delete", ""]]);
    const off = siteOf(w, [], { bulkDelete: false }).testClient(w.db, { user: SU });
    assert.equal((await post(off, "bulk_delete", { keys: ["2"], inputs: {} })).status, 404);
    assert.deepEqual(await names(w), ["b", "c"]);
  }));

  test(`bulk delete with a delete service gets the query set and a single delete keeps working (${provider})`, () => world(provider, async (w) => {
    const seen: [unknown, number, string[]][] = [];
    const config = {
      async delete(ctx: Context, qs: Loose) {
        seen.push([ctx.key, ctx.count, await sorted(qs)]);
        await qs.delete();
        await ctx.audit(undefined, { action: "trash", changes: { note: "by service" } });
      },
    };
    const client = siteOf(w, [], config).testClient(w.db, { user: SU });
    const r = await post(client, "bulk_delete", { keys: ["1", "2"], inputs: {} });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [[null, 2, ["a", "b"]]]);
    assert.deepEqual(await names(w), ["c", "d", "e"]);
    assert.equal((await client.delete("/admin/api/resources/AItem/3")).status, 200);
    assert.deepEqual(seen[1]![0], { id: 3 });
    assert.deepEqual((await audit(w)).map(([a]) => a), ["trash", "bulk_delete", "trash"]);
  }));

  // -- tabs, permissions, audit, config ---------------------------------------------------------------------------

  test(`row and bulk actions of the child run on a tab with the parent pinned (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const seen: string[][] = [];
    const double = action(
      async function double(_ctx, qs) {
        seen.push(await sorted(qs));
        await qs.update({ qty: item.qty.mul(2) });
      },
      { placement: "bulk" },
    );
    const only_detail = action(
      async function only_detail() {
        throw new Error("a tab runs bulk and row actions only");
      },
      { placement: "detail" },
    );
    const client = siteOf(w, [double, only_detail]).testClient(w.db, { user: SU });
    const tab = "/admin/api/tab-actions/AShop/1/items";
    let r = await post(client, "double", { keys: ["1", "4"], inputs: {} }, tab);
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [["a"]]);
    assert.equal((await post(client, "only_detail", { keys: ["1"], inputs: {} }, tab)).status, 404);
    r = await post(client, "double", { all: true, inputs: {} }, tab);
    assert.ok(r.status === 200 && JSON.stringify(seen[1]) === JSON.stringify(["a", "b", "c"]), r.text);
  }));

  test(`an action has a permission name and an action without one needs manage (${provider})`, () => world(provider, async (w) => {
    const refund = action(async function refund() {}, { placement: "bulk", permission: "refund" });
    const plain = action(async function plain() {}, { placement: "bulk" });
    const site = siteOf(w, [refund, plain, "export"]);
    assert.ok(site.permissions.includes("admin.refund_aitem") && site.permissions.includes("admin.export_aitem"));
    assert.ok(!site.permissions.some((p) => p.includes("plain")));
    const stranger = site.testClient(w.db, { user: { key: 99 } });
    assert.equal((await post(stranger, "refund", { keys: ["1"], inputs: {} })).status, 403);
    assert.equal((await post(stranger, "plain", { keys: ["1"], inputs: {} })).status, 403);
    assert.equal((await post(site.testClient(w.db, { user: SU }), "export", { keys: ["1"], inputs: {} })).status, 404);
  }));

  test(`the rows of one request share one group id and a bulk action writes a summary row (${provider})`, () => world(provider, async (w) => {
    const audited = action(
      async function audited(ctx, qs) {
        for (const row of await qs) await ctx.audit(row, { action: "audited_row", changes: { name: row.name } });
      },
      { placement: "bulk" },
    );
    const client = siteOf(w, [audited]).testClient(w.db, { user: SU });
    const r = await post(client, "audited", { keys: ["1", "2", "4"], inputs: {} });
    assert.equal(r.status, 200, r.text);
    const rows = await audit(w);
    assert.deepEqual(rows.map(([a]) => a), ["audited_row", "audited_row", "audited_row", "action:audited"]);
    assert.equal(new Set(rows.map((row) => row[3].request_id)).size, 1);
    assert.equal(rows[rows.length - 1]![3].count, 3);
  }));

  test(`a bigint key and a date filter reach the query set with their types (${provider})`, () => world(provider, async (w) => {
    const big = w.models["ABig"];
    const base = 2n ** 40n;
    await big.objects.using(w.db).insertMany([{ id: base, tag: "a" }, { id: base + 1n, tag: "b" }]);
    const seen: unknown[] = [];
    const tag = action(
      async function tag(_ctx, qs) {
        seen.push((await qs).map((r: Loose) => r.id).sort());
      },
      { placement: "bulk" },
    );
    const site = new Admin(w.registry);
    site.register(big, { actions: [tag] } as ResourceConfig);
    const client = site.testClient(w.db, { user: SU });
    const r = await client.post("/admin/api/actions/ABig/tag", { json: { keys: [String(base + 1n)], inputs: {} } });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(seen, [[base + 1n]]);
  }));

  test(`an action is a plain function that a unit test calls with no admin (${provider})`, () => world(provider, async (w) => {
    const item = w.models["AItem"];
    const add = action(
      async function add(ctx, qs) {
        await qs.update({ qty: item.qty.add(ctx.inputs["by"] as number) });
        return `added ${ctx.inputs["by"]}`;
      },
      { placement: "bulk", inputs: [input("by", { type: "number" })] },
    );
    const ctx = new Context({ inputs: { by: 10 }, database: w.db });
    assert.equal(await add(ctx, item.objects.using(w.db).filter(item.shopId.eq(2))), "added 10");
    assert.deepEqual(await qtys(w), [["a", 1], ["b", 2], ["c", 3], ["d", 14], ["e", 15]]);
  }));

  test(`a wrong action config is an admin config error (${provider})`, () => world(provider, async (w) => {
    const badPlace = action(async function badPlace() {}, { placement: "middle" as Loose });
    const twice = action(async function twice() {}, { placement: "bulk", confirm: "x" });
    const cases: [Loose, RegExp][] = [
      [{ actions: [badPlace] }, /placement/],
      [{ actions: [twice, twice] }, /listed twice/],
      [{ actions: [7] }, /an action is a name/],
      [{ bulkDelete: "no" }, /bulkDelete/],
    ];
    for (const [config, text] of cases) {
      const site = new Admin(w.registry);
      site.register(w.models["AItem"], config);
      assert.throws(() => site.check(), (e) => e instanceof AdminConfigError && text.test(e.message), String(text));
    }
  }));

  test(`an action writes a protected model only when the resource opts in (${provider})`, () => world(provider, async (w) => {
    const vault = w.models["AVault"];
    await allowWrites([vault], async () => {
      await vault.objects.using(w.db).insertMany([{ note: "open" }]);
    });
    const seal = action(
      async function seal(_ctx, qs) {
        await qs.update({ note: "sealed" });
        return "Sealed";
      },
      { placement: "bulk" },
    );
    const run = (optIn: boolean) => {
      const site = new Admin(w.registry);
      site.register(vault, { allowProtectedWrites: optIn, actions: [seal] } as ResourceConfig);
      return post(site.testClient(w.db, { user: SU }), "seal", { keys: ["1"] }, "/admin/api/actions/AVault");
    };
    const notes = async () => (await vault.objects.using(w.db).all()).map((r: Loose) => r.note);
    const refused = await run(false);
    assert.notEqual(refused.status, 200, refused.text);
    assert.deepEqual(await notes(), ["open"]);
    const done = await run(true);
    assert.equal(done.status, 200, done.text);
    assert.deepEqual(await notes(), ["sealed"]);
  }));
}
