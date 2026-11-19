/** Frozen schema identities through native compilation and both database drivers. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { main as cli } from "../src/cli.js";
import { connect, DatabaseError, IntegrityError, load, Migrations, Migrator, Registry, SchemaError } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { native } from "../src/native.js";
import { resolveExtensions } from "../src/packages.js";
import { scratch as scratchDir } from "./scratch.js";

// Under this package, so the schemas find the installed generic package as a direct dependency.
const scratch = scratchDir("node-identities-");

for (const [provider, url] of [
  ["sqlite", "sqlite://:memory:"],
  ["postgresql", process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test"],
] as const) {
  // case: identities::content_type_identity_roundtrip_and_frozen_generation
  test(`ContentType identity roundtrip and frozen generation (${provider})`, { skip: provider === "postgresql" && !url }, async () => {
    const dir = mkdtempSync(join(scratch, "schema-"));
    const path = join(dir, "schema.prisma");
    writeFileSync(path, `datasource db {\nprovider = "${provider}"\n}\nmodel Post {\nid Int @id\n@@map("identity08_node_posts")\n}\nmodel Tag {\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_node_tags")\n}`);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const frozen = readFileSync(join(dir, "schema.identities.json"), "utf8");
    const registry = new Registry();
    const models = load(path, { registry });
    const Post = models["Post"]!;
    const Tag = models["Tag"]!;
    const types = registry.getEnum("ContentType");
    assert.equal(types["Post"], 1);
    assert.equal(types["Tag"], 2);
    assert.deepEqual(registry.ir()["identities"], JSON.parse(frozen));
    const db = await connect(url, { registry, default: false });
    try {
      await db.dropTables();
      await db.createTables();
      const post = await Post.objects.using(db).insert({ id: 7 });
      assert.ok(post);
      const tag = await Tag.objects.using(db).insert({ id: 1, kind: types["Post"], objectId: 7 });
      assert.equal((tag as unknown as Record<string, unknown>)["kind"], types["Post"]);
      await assert.rejects(db.execute("INSERT INTO identity08_node_tags (id, kind, object_id) VALUES (2, 99, 7)"), IntegrityError);
      await (post as { delete(): Promise<void> }).delete();
      assert.equal(((await Tag.objects.using(db).get()) as unknown as Record<string, unknown>)["objectId"], 7);
      assert.equal(await cli(["--schema", path, "generate", "typescript", "-o", join(dir, "models.ts")]), 0);
      assert.match(readFileSync(join(dir, "models.ts"), "utf8"), /Post: 1/);
      assert.equal(readFileSync(join(dir, "schema.identities.json"), "utf8"), frozen);
    } finally {
      await db.dropTables();
      await db.close();
      rmSync(dir, { recursive: true });
    }
  });
}

test("missing or conflicting manifest fails during schema definition", async () => {
  const dir = mkdtempSync(join(scratch, "invalid-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, "model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\n}");
    assert.throws(() => load(path, { registry: new Registry() }), SchemaError);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const file = join(dir, "schema.identities.json");
    const value = JSON.parse(readFileSync(file, "utf8")) as { entries: { id: number }[] };
    value.entries[1]!.id = value.entries[0]!.id;
    writeFileSync(file, JSON.stringify(value));
    assert.throws(() => load(path, { registry: new Registry() }), /duplicate ContentType ID/);
  } finally { rmSync(dir, { recursive: true }); }
});

// case: generic_identities::a_generic_relation_schema_loads_and_round_trips
test("a generic relation schema loads and round-trips", async () => {
  const dir = mkdtempSync(join(scratch, "generic-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, `datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\n@@map("generic08_node_posts")\n}\nmodel Tag {\nid Int @id\ncontent_type ContentType?\nobject_id Int?\n@@generic.relation("target", type: "content_type", key: "object_id", targets: ["Post"])\n@@map("generic08_node_tags")\n}`);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const registry = new Registry();
    const models = load(path, { registry });
    const types = registry.getEnum("ContentType");
    const db = await connect("sqlite://:memory:", { registry, default: false });
    try {
      await db.createTables();
      await models["Post"]!.objects.using(db).insert({ id: 7 });
      await models["Tag"]!.objects.using(db).insert({ id: 1, contentType: types["Post"], objectId: 7 });
      const tag = await models["Tag"]!.objects.using(db).get() as unknown as Record<string, unknown>;
      assert.equal(tag["contentType"], types["Post"]);
      assert.equal(tag["objectId"], 7);
    } finally { await db.close(); }
  } finally { rmSync(dir, { recursive: true }); }
});

const genericHeader = `datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\ntags Tag[] @generic.reverse\n@@map("generic_field_node_posts")\n}\nmodel Photo {\nid Int @id\n@@map("generic_field_node_photos")\n}\n`;

// case: identities::a_generic_field_creates_its_pair_its_index_and_the_reverse_field
test("a Generic field creates its pair, its index and the reverse field", async () => {
  // A package compiles into a schema only when the schema uses its namespace, so a bare `Generic` type gets the plain error with a package hint.
  const bare = () => native().compileSchema("model Tag {\n id Int @id\n target Generic?\n}", null, resolveExtensions());
  assert.throws(bare, (e: Error) => /unknown type Generic; expected a model or/.test(e.message) && e.message.includes("Generic belongs to the built-in generic extension and needs @generic.relation"));
  const dir = mkdtempSync(join(scratch, "generic-field-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, genericHeader + `model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nnote String\n@@map("generic_field_node_tags")\n}`);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const registry = new Registry();
    const models = load(path, { registry });
    const ir = registry.ir() as unknown as { models: { name: string; fields: { name: string; nullable?: boolean; enum?: string }[]; indexes?: unknown[] }[]; behavior: Record<string, unknown> };
    const tag = ir.models.find((m) => m.name === "Tag")!;
    assert.deepEqual(tag.fields.map((f) => [f.name, f.nullable ?? false, f.enum ?? null]),
      [["id", false, null], ["target_type", true, "ContentType"], ["target_id", true, null], ["note", false, null]]);
    assert.deepEqual(tag.indexes, [{ columns: [{ field: "target_type" }, { field: "target_id" }] }]);
    const data = (ir.behavior["extension_data"] as Record<string, { relations: unknown; reverse: unknown }>)["monorm.generic"]!;
    assert.deepEqual(data.relations, [{ model: "Tag", name: "target", type_field: "target_type", key_field: "target_id", targets: ["Photo", "Post"] }]);
    assert.deepEqual(data.reverse, [{ model: "Post", name: "tags", source: "Tag", relation: "target" }]);
    const db = await connect("sqlite://:memory:", { registry, default: false });
    try {
      await db.createTables();
      await models["Tag"]!.objects.using(db).insert({ id: 1, targetType: registry.getEnum("ContentType")["Post"], targetId: 7, note: "n" });
      const row = await models["Tag"]!.objects.using(db).get() as unknown as Record<string, unknown>;
      assert.equal(row["targetType"], registry.getEnum("ContentType")["Post"]);
      assert.equal(row["targetId"], 7);
    } finally { await db.close(); }
  } finally { rmSync(dir, { recursive: true }); }
});

test("moving to the Generic field form needs no migration", async () => {
  const dir = mkdtempSync(join(scratch, "generic-move-"));
  const path = join(dir, "schema.prisma");
  const explicit = `model Tag {\nid Int @id\ntarget_type ContentType?\ntarget_id Int?\nkind ContentType\nobject_id Int\n`
    + `@@generic.relation("target", type: "target_type", key: "target_id", targets: ["Post", "Photo"])\n`
    + `@@generic.relation("owner", type: "kind", key: "object_id", targets: ["Post"])\n@@index([target_type, target_id])\n@@map("generic_field_node_tags")\n}`;
  const field = `model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nkind ContentType\nobject_id Int\n`
    + `owner Generic @generic.relation(type: "kind", key: "object_id", targets: ["Post"], index: false)\n@@map("generic_field_node_tags")\n}`;
  try {
    for (const [i, model] of [explicit, field].entries()) {
      writeFileSync(path, genericHeader.replace("tags Tag[] @generic.reverse", `tags Tag[] @generic.reverse(relation: "target")`) + model);
      assert.equal(await cli(["--schema", path, "identities"]), 0);
      const registry = new Registry();
      load(path, { registry });
      assert.equal(new Migrations(join(dir, "migrations"), registry).make("initial").length === 0, i === 1);
    }
  } finally { rmSync(dir, { recursive: true }); }
});

for (const behavior of ['{"schema_contract":2,"generic_relations":[{"model":"Tag"}]}', '{"schema_contract":2}']) {
  test(`models generated for contract 2 ask for regeneration (${behavior.length})`, () => {
    assert.throws(() => native().prepareSchema(`{"models":[],"behavior":${behavior}}`), /regenerate/);
  });
}

for (const [provider, url] of [
  ["sqlite", "sqlite://:memory:"],
  ["postgresql", process.env["ORM_TEST_DATABASE_URL"]],
] as const) {
  // case: generic_identities::retirement_migration_requires_reference_cleanup
  test(`retirement migration requires reference cleanup (${provider})`, { skip: !url }, async () => {
    const dir = mkdtempSync(join(scratch, "retire-"));
    const path = join(dir, "schema.prisma");
    const header = `datasource db {\nprovider = "${provider}"\n}\n`;
    const post = 'model Post {\nid Int @id\n@@map("identity08_migration_posts")\n}\n';
    const tag = 'model Tag {\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_migration_tags")\n}';
    writeFileSync(path, header + post + tag);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const old = new Registry();
    const models = load(path, { registry: old });
    const migrations = join(dir, "migrations");
    new Migrations(migrations, old).make("initial");
    const db = await connect(url!, { registry: old, default: false });
    const runner = new Migrator(db, new Migrations(migrations, old));
    const Tag = models["Tag"]!;
    try {
      await db.dropTables();
      // The migration ledger outlives dropTables on a shared database.
      await db.execute("DROP TABLE IF EXISTS monorm_migrations");
      await runner.upgrade("1");
      await Tag.objects.using(db).insert({ id: 1, kind: old.getEnum("ContentType")["Post"], objectId: 9 });
      writeFileSync(path, header + tag);
      assert.equal(await cli(["--schema", path, "identities"]), 0);
      const current = new Registry();
      load(path, { registry: current });
      new Migrations(migrations, current).make("retire_post");
      await assert.rejects(runner.upgrade(), DatabaseError);
      assert.equal(await Tag.objects.using(db).count(), 1);
      await Tag.objects.using(db).all().delete();
      await runner.upgrade();
      await assert.rejects(db.execute("INSERT INTO identity08_migration_tags (id, kind, object_id) VALUES (2, 1, 9)"), IntegrityError);
    } finally {
      await db.dropTables();
      await db.execute("DROP TABLE IF EXISTS monorm_migrations");
      await db.close();
      rmSync(dir, { recursive: true });
    }
  });
}

// case: generic_identities::a_proxy_that_keeps_a_generic_field_fails_and_one_without_it_loads
test("a proxy that keeps a generic field fails and one without it loads", { skip: !hasExtension("monorm.generic", "monorm.proxy") }, async () => {
  const dir = mkdtempSync(join(scratch, "generic-proxy-"));
  const path = join(dir, "schema.prisma");
  const tag = 'model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post"])\nnote String\n@@map("generic_field_node_tags")\n}\n';
  try {
    for (const fields of ['include: ["id", "note"]', 'exclude: ["target"]']) {
      writeFileSync(path, genericHeader + tag + `model NotedTag {\n@@proxy.of(Tag)\n@@proxy.fields(${fields})\n}`);
      assert.equal(await cli(["--schema", path, "identities"]), 0);
      const registry = new Registry();
      const models = load(path, { registry });
      const db = await connect("sqlite://:memory:", { registry, default: false });
      try {
        await db.createTables();
        await models["Tag"]!.objects.using(db).insert({ id: 1, targetType: registry.getEnum("ContentType")["Post"], targetId: 7, note: "n" });
        const rows = await models["NotedTag"]!.objects.using(db) as unknown as { id: number; note: string }[];
        assert.deepEqual(rows.map((t) => [t.id, t.note]), [[1, "n"]]);
      } finally { await db.close(); }
    }
    // The identity manifest of the same models stays; a full proxy keeps the Generic field.
    writeFileSync(path, genericHeader + tag + "model NotedTag {\n@@proxy.of(Tag)\n}");
    assert.throws(() => load(path, { registry: new Registry() }), /Tag\.target: NotedTag keeps the Generic field/);
  } finally { rmSync(dir, { recursive: true }); }
});
