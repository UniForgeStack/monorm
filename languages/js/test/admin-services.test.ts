// Admin writes through the bridge: host behavior, the protected opt-in, services, `ctx`, inputs and `log`, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { Admin, AdminConfigError, Context, input, log, type ResourceConfig } from "../src/admin.js";
import { connect, load, param, registerFieldAdapter, Registry, type Database } from "../src/index.js";
import { DoesNotExist, ValidationError, VersionConflict } from "../src/errors.js";
import { allowWrites } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
const PROVIDERS = ["sqlite", "postgres"] as const;

const SCHEMA = (provider: string) => `datasource db {
  provider = "${provider}"
}
import "app:admin" (prefix: "admin_")

model SNote {
  id    Int    @id @default(autoincrement())
  owner Int    @default(1)
  title String @db.VarChar(20)
  slug  String @default("")
  @@map("s_notes")
}

model SLocked {
  id Int @id @default(autoincrement())
  x  Int
  @@protected_write
  @@map("s_locked")
}

model SCode {
  code String @id
  note String @default("")
  @@map("s_codes")
}

model SBig {
  id  Int    @id @default(autoincrement())
  big BigInt @default(0)
  @@map("s_bigs")
}
`;

const NOTES = "/admin/api/resources/SNote";
const LOCKED = "/admin/api/resources/SLocked";
const SU = { key: 1, is_superuser: true };

type Loose = any;
interface World {
  readonly registry: Registry;
  readonly db: Database;
  readonly models: Record<string, Loose>;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-services-"));
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
    await models["SNote"].objects.using(db).insertMany([{ owner: 1, title: "First" }, { owner: 2, title: "Theirs" }]);
    await allowWrites([models["SLocked"]], async () => {
      await models["SLocked"].objects.using(db).insertMany([{ x: 1 }]);
    });
    await run({ registry, db, models });
  } finally {
    await db.dropTables();
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

const audit = async (w: World): Promise<Loose[]> => {
  const entry = w.models["AdminLogEntry"];
  return await entry.objects.using(w.db).orderBy(entry.id);
};

function siteOf(w: World, note: ResourceConfig = {}, locked: ResourceConfig = {}): Admin {
  const site = new Admin(w.registry);
  site.register(w.models["SNote"], note);
  site.register(w.models["SLocked"], locked);
  return site;
}

const lower = {
  validate: (value: string) => value.toLowerCase(),
  prepare: async (value: string) => value,
  decode: (stored: string) => stored,
};

function services(w: World): { config: ResourceConfig; seen: Record<string, unknown> } {
  const Note = w.models["SNote"];
  const seen: Record<string, unknown> = {};
  const config: ResourceConfig = {
    async create(ctx, values) {
      seen["create"] = [ctx.user["key"], typeof ctx.requestId, { ...ctx.inputs }, { ...ctx.loaded }];
      const db = ctx.database!;
      return await db.transaction(async () => {
        const made = await Note.objects.using(db).insert({ ...values, owner: ctx.user["key"] });
        await ctx.audit(made, { changes: { reason: ctx.inputs["reason"] } });
        return made;
      });
    },
    async update(ctx, qs, changes) {
      await ctx.database!.transaction(async () => {
        const row = await qs.first();
        if (row === null) throw new DoesNotExist("gone");
        if (row.title !== ctx.loaded["title"]) throw new VersionConflict("stale");
        if (changes.title === "bad") throw new ValidationError({ title: "Not this title." }, { nonFieldMessages: ["Fix the form."] });
        await qs.update(changes);
        await ctx.audit(undefined, { changes: { reason: ctx.inputs["reason"] } });
      });
    },
    async delete(ctx, qs) {
      await ctx.database!.transaction(async () => {
        await qs.delete();
      });
    },
    inputs: [input("reason", { type: "textarea", required: true, label: "Reason" })],
  };
  return { config, seen };
}

for (const provider of PROVIDERS) {
  test(`a query set write with the queryset and the can rule for a model with host behavior (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    registerFieldAdapter(Note._meta, "slug", lower);
    const config = { queryset: Note.objects.filter(Note.owner.eq(param("user"))).prepare(), can: { change: Note.title.ne("Locked") } };
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const r = await client.patch(`${NOTES}/1`, { json: { set: { title: "Renamed", slug: "AbC" }, loaded: { title: "First", slug: "" } } });
    assert.equal(r.status, 200, r.text);
    const row = await Note.objects.using(w.db).filter(Note.id.eq(1)).first();
    assert.deepEqual([row.title, row.slug], ["Renamed", "abc"]);
    const other = await client.patch(`${NOTES}/2`, { json: { set: { title: "Mine" }, loaded: { title: "Theirs" } } });
    assert.equal(other.status, 404);
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(2)).first()).title, "Theirs");
    const rows = await audit(w);
    assert.deepEqual(rows.map((e) => [e.action, e.recordKey]), [["update", "1"]]);
    assert.equal(rows[0].changes.service, false);
  }));

  test(`a protected model stays closed until the resource opts in and the audit row records it (${provider})`, () => world(provider, async (w) => {
    const Locked = w.models["SLocked"];
    const closed = siteOf(w).testClient(w.db, { user: SU });
    const denied = await closed.patch(`${LOCKED}/1`, { json: { set: { x: 2 }, loaded: { x: 1 } } });
    assert.deepEqual([denied.status, denied.json().error.kind], [403, "ReadOnly"]);
    const opened = siteOf(w, {}, { allowProtectedWrites: true }).testClient(w.db, { user: SU });
    const r = await opened.patch(`${LOCKED}/1`, { json: { set: { x: 2 }, loaded: { x: 1 } } });
    assert.equal(r.status, 200, r.text);
    assert.equal((await Locked.objects.using(w.db).filter(Locked.id.eq(1)).first()).x, 2);
    const created = await opened.post(LOCKED, { json: { set: { x: 7 } } });
    assert.equal(created.status, 201, created.text);
    const rows = await audit(w);
    assert.deepEqual(rows.map((e) => [e.action, e.changes.allow_protected_writes]), [["update", true], ["create", true]]);
  }));

  test(`async services create, update and delete with ctx, inputs and one audit row each (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    const { config, seen } = services(w);
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const made = await client.post(NOTES, { json: { set: { title: "Made" }, inputs: { reason: "because" } } });
    assert.equal(made.status, 201, made.text);
    assert.equal(made.json().key, "3");
    assert.deepEqual(seen["create"], [1, "string", { reason: "because" }, {}]);
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(3)).first()).owner, 1);
    const edited = await client.patch(`${NOTES}/3`, { json: { set: { title: "Edited" }, loaded: { title: "Made" }, inputs: { reason: "typo" } } });
    assert.equal(edited.status, 200, edited.text);
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(3)).first()).title, "Edited");
    const gone = await client.delete(`${NOTES}/3`, { json: { loaded: { title: "Edited" }, inputs: { reason: "gone" } } });
    assert.equal(gone.status, 200, gone.text);
    assert.equal(await Note.objects.using(w.db).filter(Note.id.eq(3)).first(), null);
    const rows = await audit(w);
    assert.deepEqual(rows.map((e) => [e.action, e.recordKey]), [["create", "3"], ["update", "3"], ["delete", "3"]]);
    assert.equal(rows[0].changes.reason, "because");
    assert.equal(rows[0].changes.service, true);
    assert.deepEqual(rows[0].changes.inputs, { reason: "because" });
    assert.equal(rows[1].changes.reason, "typo");
  }));

  test(`a version conflict is a 409 with the current values and a validation error is a 422 (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    const { config } = services(w);
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const conflict = await client.patch(`${NOTES}/1`, { json: { set: { title: "Mine" }, loaded: { title: "Old" }, inputs: { reason: "x" } } });
    assert.equal(conflict.status, 409, conflict.text);
    assert.equal(conflict.json().current.title, "First");
    assert.equal((await Note.objects.using(w.db).filter(Note.id.eq(1)).first()).title, "First");
    const bad = await client.patch(`${NOTES}/1`, { json: { set: { title: "bad" }, loaded: { title: "First" }, inputs: { reason: "x" } } });
    assert.equal(bad.status, 422, bad.text);
    assert.deepEqual([bad.json().fields.title, bad.json().form], [["Not this title."], ["Fix the form."]]);
    assert.deepEqual(await audit(w), []);
  }));

  test(`a required input is a 422 and a service that does not exist for the row is a 404 (${provider})`, () => world(provider, async (w) => {
    const { config } = services(w);
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const missing = await client.post(NOTES, { json: { set: { title: "Made" } } });
    assert.equal(missing.status, 422, missing.text);
    assert.ok("reason" in missing.json().fields);
    const none = await client.patch(`${NOTES}/99`, { json: { set: { title: "x" }, loaded: { title: "y" }, inputs: { reason: "x" } } });
    assert.equal(none.status, 404);
  }));

  test(`a service runs with the queryset filter and the can rule on its query set (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    const { config } = services(w);
    const scoped = { ...config, queryset: Note.objects.filter(Note.owner.eq(param("user"))).prepare(), can: { change: Note.title.ne("First") } };
    const client = siteOf(w, scoped).testClient(w.db, { user: SU });
    const body = { set: { title: "Z" }, inputs: { reason: "x" } };
    assert.equal((await client.patch(`${NOTES}/2`, { json: { ...body, loaded: { title: "Theirs" } } })).status, 404);
    assert.equal((await client.patch(`${NOTES}/1`, { json: { ...body, loaded: { title: "First" } } })).status, 403);
  }));

  test(`a service that fails after ctx audit leaves no row and no audit row (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    const config: ResourceConfig = {
      async create(ctx, values) {
        const db = ctx.database!;
        return await db.transaction(async () => {
          const made = await Note.objects.using(db).insert(values);
          await ctx.audit(made);
          throw new Error("after the audit");
        });
      },
    };
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const r = await client.post(NOTES, { json: { set: { title: "Made" } } });
    assert.equal(r.status, 500, r.text);
    assert.equal(await Note.objects.using(w.db).count(), 2);
    assert.deepEqual(await audit(w), []);
  }));

  test(`a secret input is masked in the audit row and the service row gets a label (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    const seen: Record<string, unknown> = {};
    const config: ResourceConfig = {
      async create(ctx, values) {
        seen["pw"] = ctx.inputs["pw"];
        const db = ctx.database!;
        return await db.transaction(async () => {
          const made = await Note.objects.using(db).insert(values);
          await ctx.audit(made);
          return made;
        });
      },
      inputs: [input("pw", { secret: true }), input("reason")],
    };
    const client = siteOf(w, config).testClient(w.db, { user: SU });
    const r = await client.post(NOTES, { json: { set: { title: "Made" }, inputs: { pw: "hunter2", reason: "why" } } });
    assert.equal(r.status, 201, r.text);
    assert.equal(seen["pw"], "hunter2");
    const rows = await audit(w);
    assert.equal(rows.length, 1);
    assert.deepEqual(rows[0].changes.inputs, { pw: "***", reason: "why" });
    assert.ok(!JSON.stringify(rows[0].changes).includes("hunter2"));
    assert.equal(rows[0].recordLabel, "Made");
  }));

  test(`a create with the default writer needs the required fields (${provider})`, () => world(provider, async (w) => {
    const Note = w.models["SNote"];
    registerFieldAdapter(Note._meta, "slug", lower);
    const client = siteOf(w).testClient(w.db, { user: SU });
    const r = await client.post(NOTES, { json: { set: { slug: "A" } } });
    assert.equal(r.status, 422, r.text);
    assert.deepEqual(r.json().fields.title, ["This field is required."]);
    assert.equal(await Note.objects.using(w.db).count(), 2);
  }));

  test(`a plain sync service and an input without a service are site build errors (${provider})`, () => world(provider, async (w) => {
    assert.throws(() => siteOf(w, { inputs: [input("reason")] }).check(), (e: unknown) => e instanceof AdminConfigError && /service/.test(e.message));
    const { config } = services(w);
    assert.throws(() => siteOf(w, { update: config.update!, inputs: [input("kind", { type: "select" })] }).check(), (e: unknown) => e instanceof AdminConfigError && /select/.test(e.message));
    siteOf(w, { update: config.update! }).check();
  }));

  test(`a service is a plain function that a unit test calls with no admin (${provider})`, () => world(provider, async (w) => {
    const { config } = services(w);
    const ctx = new Context({ user: { key: 7 }, requestId: "r1", inputs: { reason: "unit" }, database: w.db });
    const made = await config.create!(ctx, { title: "Unit" }) as Loose; // no admin: the context is built by hand
    assert.deepEqual([made.owner, made.title], [7, "Unit"]);
    const rows = await audit(w);
    assert.deepEqual(rows.map((e) => [e.userKey, e.action, e.resource, e.recordKey]), [["7", "service", "SNote", String(made.id)]]);
    assert.equal(rows[0].changes.request_id, "r1");
  }));

  test(`admin log lets host code write an audit row in the open transaction (${provider})`, () => world(provider, async (w) => {
    await log("export", { user: { key: 5 }, resource: "SNote", recordKey: 3, label: "Q3", changes: { rows: 10 }, using: w.db });
    await w.db.transaction(async () => {
      await log("purge", { user: 9, using: w.db });
    });
    const rows = await audit(w);
    assert.deepEqual(rows.map((e) => [e.userKey, e.action, e.resource, e.recordKey, e.recordLabel]), [["5", "export", "SNote", "3", "Q3"], ["9", "purge", "", "", ""]]);
    assert.deepEqual(rows[0].changes, { rows: 10 });
  }));

  test(`a service audit on a key that needs escaping gets the rust key text and the label (${provider})`, () => world(provider, async (w) => {
    const Code = w.models["SCode"];
    const config: ResourceConfig = {
      async create(ctx, values) {
        const db = ctx.database!;
        return await db.transaction(async () => {
          const made = await Code.objects.using(db).insert(values);
          await ctx.audit(made);
          return made;
        });
      },
      async update(ctx, qs, changes) {
        await ctx.database!.transaction(async () => {
          await qs.update(changes);
          await ctx.audit();
        });
      },
    };
    const site = new Admin(w.registry);
    site.register(Code, config);
    const client = site.testClient(w.db, { user: SU });
    for (const key of ["a b", "a,b"]) {
      const made = await client.post("/admin/api/resources/SCode", { json: { set: { code: key, note: "n" } } });
      assert.equal(made.status, 201, made.text);
      const r = await client.patch(`/admin/api/resources/SCode/${made.json().key}`, { json: { set: { note: "m" }, loaded: { note: "n" } } });
      assert.equal(r.status, 200, r.text);
    }
    const rows = await audit(w);
    assert.deepEqual(rows.map((a) => [a.action, a.recordKey]), [["create", "a%20b"], ["update", "a%20b"], ["create", "a%2Cb"], ["update", "a%2Cb"]]);
    assert.ok(rows.every((a) => a.recordLabel !== ""));
  }));

  test(`a bigint value sent as a string reaches the service as an int (${provider})`, () => world(provider, async (w) => {
    const Big = w.models["SBig"];
    const seen: Record<string, unknown> = {};
    const config: ResourceConfig = {
      async create(ctx, values) {
        seen["big"] = values["big"];
        return await Big.objects.using(ctx.database!).insert(values);
      },
    };
    const site = new Admin(w.registry);
    site.register(Big, config);
    const r = await site.testClient(w.db, { user: SU }).post("/admin/api/resources/SBig", { json: { set: { big: "9007199254740993" } } });
    assert.equal(r.status, 201, r.text);
    assert.equal(seen["big"], 9007199254740993n);
  }));

  test(`a route audit gets the rust key text of the model of its record (${provider})`, () => world(provider, async (w) => {
    const Code = w.models["SCode"];
    const site = new Admin(w.registry);
    site.register(Code, {});
    site.route("POST", "stamp", async (ctx, request) => {
      const made = await Code.objects.using(ctx.database!).insert({ code: (request.body as { code: string }).code });
      const child = await w.models["SNote"].objects.using(ctx.database!).insert({ title: "child" });
      await ctx.audit(made);
      await ctx.audit(child);
      await ctx.audit();
      return { ok: true };
    });
    const client = site.testClient(w.db, { user: SU });
    for (const key of ["a b", "a,b"]) {
      const r = await client.post("/admin/api/routes/stamp", { json: { code: key } });
      assert.equal(r.status, 200, r.text);
    }
    const rows = await audit(w);
    const children = (await w.models["SNote"].objects.using(w.db).filter(w.models["SNote"].title.eq("child")).orderBy(w.models["SNote"].id)).map((n: { id: unknown }) => String(n.id));
    assert.deepEqual(rows.map((a) => [a.action, a.resource, a.recordKey]), [
      ["service", "SCode", "a%20b"], ["service", "SNote", children[0]], ["service", "", ""],
      ["service", "SCode", "a%2Cb"], ["service", "SNote", children[1]], ["service", "", ""],
    ]);
  }));

  test(`a bigint value that is not a number is a 422 that names the field (${provider})`, () => world(provider, async (w) => {
    const Big = w.models["SBig"];
    let calls = 0;
    const config: ResourceConfig = {
      async create(ctx, values) {
        calls++;
        return await Big.objects.using(ctx.database!).insert(values);
      },
    };
    const site = new Admin(w.registry);
    site.register(Big, config);
    const r = await site.testClient(w.db, { user: SU }).post("/admin/api/resources/SBig", { json: { set: { big: "abc" } } });
    assert.equal(r.status, 422, r.text);
    assert.deepEqual((r.json() as { fields: Record<string, string[]> }).fields["big"], ["Must be a whole number"]);
    assert.equal(calls, 0);
  }));
}
