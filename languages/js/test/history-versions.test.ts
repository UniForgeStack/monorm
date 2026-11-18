/** `@@history.versions`: numbered versions of an aggregate, dedupe, timeline, version_at and restore; needs the `monorm.history` package. */

import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { connect, loads, Migrations, Registry, scope, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

const SCHEMA = `
model BundleVersion {
  id Int @id @default(autoincrement())
  bundleId Int @map("bundle_id")
  @@history.versions(of: bundleId, scope: [user])
  @@map("hv_js_bundle_versions")
}
model PlainVersion {
  id Int @id @default(autoincrement())
  docId Int @map("doc_id")
  @@history.versions(of: docId)
  @@map("hv_js_plain_versions")
}
`;

const skip = !hasExtension("monorm.history", "monorm.timestamps");
// The built package: the tests run from dist/test, and the extension is assembled next to dist.
const specifier = "../../extensions/history/index.js";
const history: (model: Any) => Any = skip ? () => undefined : ((await import(specifier)) as Any).history;
const urlOf = (provider: string) => (provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test"));

type State = Record<number, Record<string, unknown>>;

/** A stand-in for the aggregate: the state per id, and the restore calls. */
class Store {
  state: State = {};
  restored: Record<string, unknown>[] = [];
  generate = (entity: Any): Record<string, unknown> => ({ ...this.state[typeof entity === "object" ? entity.id : entity]! });
  restore = (snapshot: Record<string, unknown>): void => {
    this.restored.push(snapshot);
    this.state[snapshot["id"] as number] = { ...snapshot };
  };
}

async function withSchema(provider: string, body: (db: Database, models: Record<string, Any>, store: Store) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const models = loads(`datasource db {\n  provider = "${provider}"\n}\n${SCHEMA}`, { registry }) as Record<string, Any>;
  const db = await connect(urlOf(provider), { registry, default: false });
  await db.dropTables();
  await db.createTables();
  const store = new Store();
  history(models["BundleVersion"]).register({ generate: store.generate, restore: store.restore });
  try {
    await body(db, models, store);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

for (const provider of ["sqlite", "postgresql"]) {
  const scenario = (what: string, body: (db: Database, models: Record<string, Any>, store: Store) => Promise<void>) => test(what, { skip }, () => withSchema(provider, body));
  const sqlite = provider === "sqlite";

  scenario("the version model has the version columns", async (db, models) => {
    const names = new Set<string>(models["BundleVersion"]._meta.inputFields.keys());
    for (const name of ["versionNumber", "snapshot", "contentHash", "effectiveAt", "changeType", "restoredFrom", "xactId", "user"]) assert.ok(names.has(name), name);
    assert.ok(!models["PlainVersion"]._meta.inputFields.has("user"));
  });

  scenario("record numbers the versions of each entity from one", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 1: { id: 1, title: "a" }, 2: { id: 2, title: "x" } };
    const first = await h.record(1, "create");
    store.state[1]!["title"] = "b";
    const second = await h.record(1, "update");
    const other = await h.record(2, "create");
    assert.deepEqual([first.versionNumber, second.versionNumber, other.versionNumber], [1, 2, 1]);
    assert.deepEqual([first.changeType, second.changeType], ["create", "update"]);
    assert.deepEqual(second.snapshot, { id: 1, title: "b" });
    assert.equal(second.contentHash.length, 64);
  });

  scenario("a snapshot with the hash of the latest version writes nothing", async (db, models, store) => {
    const BundleVersion = models["BundleVersion"];
    const h = history(BundleVersion).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    const first = await h.record(1, "create");
    const same = await h.record(1, "update");
    assert.equal(same.id, first.id);
    assert.equal(await BundleVersion.objects.using(db).count(), 1);
    store.state[1]!["title"] = "b";
    assert.equal((await h.record(1, "update")).versionNumber, 2);
    store.state[1]!["title"] = "a";
    assert.equal((await h.record(1, "update")).versionNumber, 3);
  });

  scenario("a delete version is written even when the snapshot is the same", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    await h.record(1, "create");
    const gone = await h.record(1, "delete");
    assert.deepEqual([gone.versionNumber, gone.changeType], [2, "delete"]);
  });

  scenario("a version after a delete is written even when the snapshot is the same", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    await h.record(1, "create");
    await h.record(1, "delete");
    const again = await h.record(1, "create");
    assert.deepEqual([again.versionNumber, again.changeType], [3, "create"]);
    assert.equal((await h.record(1, "update")).versionNumber, 3);
  });

  scenario("the content hash is the sha256 of the canonical snapshot", async (db, models, store) => {
    store.state = { 1: { title: "a", id: 1 } };
    const version = await history(models["BundleVersion"]).using(db).record(1, "create");
    assert.equal(version.contentHash, "b7962b1130324bd10b5b262d3975757cb24a2a216bc9b5dd87a2ab5a0fe432fd");
  });

  scenario("record takes an object with an id", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 7: { id: 7 } };
    assert.equal((await h.record({ id: 7 }, "create")).bundleId, 7);
  });

  scenario("record before register raises", async (db, models) => {
    await assert.rejects(history(models["PlainVersion"]).using(db).record(1, "create"), /not registered/);
  });

  scenario("record rejects a change type that restore owns", async (db, models, store) => {
    store.state = { 1: { id: 1 } };
    await assert.rejects(history(models["BundleVersion"]).using(db).record(1, "restore" as Any), /changeType/);
  });

  scenario("timeline gives the versions by number and the diff with the previous one", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 1: { id: 1, title: "a", qty: 1 } };
    await h.record(1, "create");
    Object.assign(store.state[1]!, { title: "b", qty: 2 });
    await h.record(1, "update");
    const entries = await h.timeline(1, { diff: true });
    assert.deepEqual(entries.map((e: Any) => e.event.versionNumber), [1, 2]);
    assert.equal(entries[0]!.diff, null);
    assert.deepEqual(entries[1]!.diff, { title: { old: "a", new: "b" }, qty: { old: 1, new: 2 } });
    assert.equal((await h.timeline(1))[1]!.diff, null);
  });

  scenario("version at gives the last version at or before a time", async (db, models, store) => {
    const BundleVersion = models["BundleVersion"];
    const h = history(BundleVersion).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    const first = await h.record(1, "create");
    store.state[1]!["title"] = "b";
    const second = await h.record(1, "update");
    // SQLite stamps whole seconds, so the test moves the second version a day on.
    const day = 24 * 3600 * 1000;
    const base = (first.effectiveAt as Date).getTime();
    await BundleVersion.objects.using(db).filter(BundleVersion.id.eq(second.id)).update({ effectiveAt: new Date(base + day) });
    assert.equal((await h.versionAt(1, new Date(base + 3600 * 1000))).id, first.id);
    assert.equal((await h.versionAt(1, new Date(base + 2 * day))).id, second.id);
    assert.equal(await h.versionAt(1, new Date(base - day)), null);
    assert.equal((await h.at(1, new Date(base + 3600 * 1000))).id, first.id);
  });

  scenario("restore calls the function and leaves a restore version", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    const first = await h.record(1, "create");
    store.state[1]!["title"] = "b";
    await h.record(1, "update");
    const restored = await h.restore(first);
    assert.deepEqual(store.restored, [{ id: 1, title: "a" }]);
    assert.equal(store.state[1]!["title"], "a");
    assert.deepEqual([restored.versionNumber, restored.changeType, restored.restoredFrom], [3, "restore", first.id]);
    assert.deepEqual(restored.snapshot, { id: 1, title: "a" });
  });

  scenario("a restore function that raises writes no version", async (db, models, store) => {
    const BundleVersion = models["BundleVersion"];
    const h = history(BundleVersion).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    const first = await h.record(1, "create");
    h.register({
      generate: store.generate,
      restore: () => {
        throw new Error("no");
      },
    });
    await assert.rejects(h.restore(first), /no/);
    assert.equal(await BundleVersion.objects.using(db).count(), 1);
  });

  scenario("a version rolls back with the outer transaction", async (db, models, store) => {
    const BundleVersion = models["BundleVersion"];
    const h = history(BundleVersion).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    await assert.rejects(
      db.transaction(async () => {
        await h.record(1, "create");
        throw new Error("stop");
      }),
      /stop/,
    );
    assert.equal(await BundleVersion.objects.using(db).count(), 0);
    assert.equal((await h.record(1, "create")).versionNumber, 1);
  });

  scenario("the unique index refuses a repeated number", async (db, models, store) => {
    const BundleVersion = models["BundleVersion"];
    store.state = { 1: { id: 1 } };
    const first = await history(BundleVersion).using(db).record(1, "create");
    await assert.rejects(
      BundleVersion.objects.using(db).insert({ bundleId: 1, versionNumber: 1, snapshot: {}, contentHash: "x", effectiveAt: first.effectiveAt, changeType: "update" }),
    );
  });

  scenario("the scope values are stored on postgresql and null without a value", async (db, models, store) => {
    const h = history(models["BundleVersion"]).using(db);
    store.state = { 1: { id: 1, title: "a" } };
    const first: Any = await scope({ user: "ann" }, () => h.record(1, "create"));
    store.state[1]!["title"] = "b";
    const second = await h.record(1, "update");
    assert.equal(second.user, null);
    assert.equal(first.user, sqlite ? null : "ann");
    assert.equal(first.xactId === null, sqlite);
  });
}

test("concurrent writers of one entity get distinct numbers on postgresql", { skip: skip || !process.env["ORM_TEST_DATABASE_URL"] }, () =>
  withSchema("postgresql", async (db, models, store) => {
    const BundleVersion = models["BundleVersion"];
    const h = history(BundleVersion).using(db);
    let n = 0;
    h.register({ generate: () => ({ n: n++ }), restore: store.restore });
    const write = () =>
      db.transaction(async () => {
        const version = await h.record(1, "update");
        await sleep(100);
        return version;
      });
    const results = await Promise.all([write(), write(), write()]);
    assert.deepEqual(results.map((r: Any) => r.versionNumber).sort(), [1, 2, 3]);
    assert.equal(await BundleVersion.objects.using(db).count(), 3);
    // generate runs under the lock, so a later number never holds an older snapshot.
    const byNumber = [...results].sort((a: Any, b: Any) => a.versionNumber - b.versionNumber);
    assert.deepEqual(byNumber.map((r: Any) => r.snapshot.n), [0, 1, 2]);
  }),
);

test("a version model cannot also be a tracker", { skip }, () => {
  const source = `datasource db {
  provider = "sqlite"
}
model Both {
  id Int @id @default(autoincrement())
  bundleId Int @map("bundle_id")
  @@history.versions(of: bundleId)
  @@history.track
  @@map("hv_js_both")
}`;
  assert.throws(() => loads(source, { registry: new Registry() }), /history/);
});

test("of must name a field of the model", { skip }, () => {
  const source = `datasource db {
  provider = "sqlite"
}
model Bad {
  id Int @id @default(autoincrement())
  @@history.versions(of: nope)
  @@map("hv_js_bad")
}`;
  assert.throws(() => loads(source, { registry: new Registry() }), /nope/);
});

test("of cannot be the primary key", { skip }, () => {
  const source = `datasource db {
  provider = "sqlite"
}
model Bad {
  id Int @id @default(autoincrement())
  @@history.versions(of: id)
  @@map("hv_js_bad")
}`;
  assert.throws(() => loads(source, { registry: new Registry() }), /primary key/);
});

test("migrations create the version table and the unique index", { skip }, () => {
  const registry = new Registry();
  loads(`datasource db {\n  provider = "postgresql"\n}\n${SCHEMA}`, { registry });
  const up = new Migrations(mkdtempSync(join(tmpdir(), "monorm-history-versions-")), registry).plan().up.map((x) => x.sql);
  const table = up.find((x) => x.startsWith('CREATE TABLE "hv_js_bundle_versions"'));
  assert.ok(table !== undefined);
  for (const column of ["version_number", "snapshot", "content_hash", "effective_at", "change_type", "restored_from", "xact_id", "user"]) assert.ok(table.includes(`"${column}"`), column);
  assert.ok(up.includes('CREATE UNIQUE INDEX "hv_js_bundle_versions_bundle_id_version_key" ON "hv_js_bundle_versions" ("bundle_id", "version_number")'));
  assert.ok(table.includes('CONSTRAINT "hv_js_bundle_versions_restored_from_fkey" FOREIGN KEY ("restored_from") REFERENCES "hv_js_bundle_versions" ("id") ON DELETE SET NULL'));
});

test("migrations give restored_from a self reference foreign key on SQLite", { skip }, () => {
  const registry = new Registry();
  loads(`datasource db {\n  provider = "sqlite"\n}\n${SCHEMA}`, { registry });
  const up = new Migrations(mkdtempSync(join(tmpdir(), "monorm-history-versions-")), registry).plan().up.map((x) => x.sql).join("\n");
  assert.ok(up.includes('CONSTRAINT "hv_js_bundle_versions_restored_from_fkey" FOREIGN KEY ("restored_from") REFERENCES "hv_js_bundle_versions" ("id") ON DELETE SET NULL'));
});
