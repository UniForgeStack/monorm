import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import { connect, loads, NotLoaded, Registry } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";
import { scratch } from "./scratch.js";

const source = readFileSync(resolve(process.cwd(), "../../fixtures/proxy.prisma"), "utf8").replaceAll("proxy_05_", "proxy_05_node_");
type Dynamic = any; // eslint-disable-line @typescript-eslint/no-explicit-any
const enabled = hasExtension("monorm.proxy");
// The engine prints the shape warnings to fd 2, so a child process runs the reads and the parent reads its stderr.
const warningChild = (dialect: string) => `
import { readFileSync } from "node:fs";
import { connect, loads, Registry } from ${JSON.stringify(pathToFileURL(join(import.meta.dirname, "..", "src", "index.js")).href)};
const registry = new Registry();
const models = loads((${JSON.stringify(dialect)} === "sqlite" ? 'datasource db { provider = "sqlite" }\\n' : "") + readFileSync(process.argv[2], "utf8"), { registry });
const { User, Active, Post } = models;
const db = await connect(process.env.CHILD_URL, { registry, default: false });
await db.createTables();
try {
  await User.objects.using(db).insertMany([{ id: 1, name: null, status: "old" }, { id: 2, name: null, status: "old" }, { id: 3, name: "ok", status: "active" }]);
  process.stderr.write("@@read\\n");
  await Active.objects.using(db).all();
  process.stderr.write("@@count\\n");
  await Active.objects.using(db).count();
  process.stderr.write("@@load\\n");
  await Post.objects.using(db).insert({ id: 1, userId: 1 });
  process.stderr.write("@@load2\\n");
  await Post.objects.using(db).load(Post.user).all();
  process.stderr.write("@@end\\n");
} finally { await db.dropTables(); await db.close(); }
`;
for (const dialect of ["sqlite", "postgres"]) {
  // case: proxy::proxy_warning_rows_defaults_writes_and_relation_targets
  test(`proxy rows, client defaults, writes and relation targets (${dialect})`, { skip: !enabled }, async () => {
    const registry = new Registry();
    const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source, { registry });
    assert.throws(() => loads(source.replace("references: [id]", "references: [missing]"), { registry: new Registry() }), /:\d+:\d+: relation \w+\.\w+: \w+ has no field missing after extension lowering/);
    const User = models.User as Dynamic, Active = models.Active as Dynamic, Post = models.Post as Dynamic;
    const name = Active._meta.fields.get("name")!;
    assert.ok(!name.hasServerValue && name.hasInsertDefault);
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const dir = scratch("monorm-proxy-warnings-");
    writeFileSync(join(dir, "child.mjs"), warningChild(dialect));
    writeFileSync(join(dir, "proxy.prisma"), source.replaceAll("proxy_05_node_", "proxy_05_node_warn_"));
    const ran = spawnSync(process.execPath, [join(dir, "child.mjs"), join(dir, "proxy.prisma")], { encoding: "utf8", env: { ...process.env, CHILD_URL: dialect === "sqlite" ? `sqlite://${join(dir, "w.db")}` : url } });
    assert.equal(ran.status, 0, ran.stderr);
    const segment = (from: string, to: string) => ran.stderr.slice(ran.stderr.indexOf(from) + from.length, ran.stderr.indexOf(to));
    const shape = (text: string) => text.split("\n").filter((l) => l.startsWith('{"code":"monorm.proxy.shape"')).map((l) => JSON.parse(l));
    const warnings = shape(segment("@@read\n", "@@count\n"));
    assert.ok(warnings.length === 2 && warnings.every((w) => w.occurrence_count === 2 && w.model === "Active"));
    assert.equal(segment("@@count\n", "@@load\n"), "");
    assert.equal(shape(segment("@@load2\n", "@@end\n")).length, 2);
    const db = await connect(url, { registry, default: false });
    await db.createTables();
    try {
      await User.objects.using(db).insertMany([{ id: 1, name: null, status: "old" }, { id: 2, name: null, status: "old" }, { id: 3, name: "ok", status: "active" }]);
      const rows = await Active.objects.using(db).orderBy(Active.id).all() as { name: string | null; status: string }[];
      assert.equal(rows.length, 3);
      assert.equal(rows[0]!.name, null);
      assert.equal(rows[0]!.status, "old");
      assert.equal(await Active.objects.using(db).count(), 3);
      const created = await Active.objects.using(db).insert({ id: 4 }) as { name: string; status: string; settings: unknown; active: boolean; volume: number };
      assert.equal(created.name, "client"); assert.equal(created.status, "active");
      assert.deepEqual(created.settings, { labels: ["proxy", null], limit: 3 });
      assert.equal(created.active, true); assert.equal(created.volume, 7);
      const root = await User.objects.using(db).insert({ id: 5 }) as { name: null; status: string };
      assert.equal(root.name, null); assert.equal(root.status, "old");
      const changed = await Active.objects.using(db).filter(Active.id.eq(4)).update({ name: null, status: "old" }).returning() as { name: null; status: string }[];
      assert.ok(changed[0]!.name === null && changed[0]!.status === "old");
      await Post.objects.using(db).insert({ id: 1, userId: 1 });
      const posts = await Post.objects.using(db).load(Post["user"] as never).all() as { user: { name: null; status: string } }[];
      assert.equal(posts[0]!.user.name, null); assert.equal(posts[0]!.user.status, "old");
      assert.equal((posts[0]!.user as { constructor: { name: string } }).constructor.name, "Active");
      await Active.objects.using(db).filter(Active.id.eq(4)).delete();
      assert.equal(await User.objects.using(db).filter(User.id.eq(4)).count(), 0);
    } finally { await db.dropTables(); await db.close(); }
  });
}

const fieldsSource = `
model Member {
  id     Int     @id
  name   String
  status String  @default("new")
  legacy String?
  @@map("proxy_fields_node_members")
}
model Current {
  name String @client_default("anon")
  @@proxy.of(Member)
  @@proxy.fields(exclude: ["legacy", "status"])
}
model Named {
  @@proxy.of(Current)
  @@proxy.fields(include: ["id", "name"])
}
`;
for (const dialect of ["sqlite", "postgres"]) {
  test(`omitted proxy fields are not part of the model (${dialect})`, { skip: !enabled }, async () => {
    const registry = new Registry();
    const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + fieldsSource, { registry });
    const Member = models.Member!, Current = models.Current!, Named = models.Named!;
    assert.deepEqual([...Current._meta.fields.keys()], ["id", "name"]);
    assert.deepEqual([...Named._meta.fields.keys()], ["id", "name"]);
    assert.equal((Current as Record<string, unknown>).legacy, undefined);
    assert.notEqual((Member as Record<string, unknown>).legacy, undefined);
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.createTables();
    try {
      await Member.objects.using(db).insert({ id: 1, name: "ann", status: "old", legacy: "x" });
      const rows = await Current.objects.using(db).all() as object[];
      assert.deepEqual(Object.keys(rows[0]!), ["id", "name"]);
      await assert.rejects(Current.objects.using(db).insert({ id: 2, legacy: "y" } as never), /Current has no field "legacy"/);
      const created = await Current.objects.using(db).insert({ id: 2 }) as { name: string };
      assert.equal(created.name, "anon");
      const stored = await Member.objects.using(db).filter((Member as unknown as { id: { eq(v: number): never } }).id.eq(2)).get() as { status: string; legacy: null };
      assert.equal(stored.status, "new"); assert.equal(stored.legacy, null);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("proxy field rules fail at definition", { skip: !enabled }, () => {
  const cases: [string, RegExp][] = [
    ['@@proxy.fields(exclude: ["name"])', /Current\.name: a NOT NULL field without a database default cannot be omitted/],
    ['@@proxy.fields(include: ["id"], exclude: ["legacy"])', /include and exclude together/],
    ['@@proxy.nonNull("legacy")', /proxy\.nonNull/],
  ];
  for (const [attribute, message] of cases) {
    assert.throws(() => loads(fieldsSource.replace('@@proxy.fields(exclude: ["legacy", "status"])', attribute), { registry: new Registry() }), message);
  }
});

const rolesSource = `
model Doc {
  id      Int       @id
  title   String
  @@soft_delete.deleted_at("deleted", mode: "application")
  @@locking.version
  @@map("proxy_role_docs")
}
model Draft {
  @@proxy.of(Doc)
}
`;
const rolesEnabled = hasExtension("monorm.proxy", "monorm.soft-delete", "monorm.locking");
for (const dialect of ["sqlite", "postgres"]) {
  test(`a proxy takes the roles of its root (${dialect})`, { skip: !rolesEnabled }, async () => {
    const registry = new Registry();
    const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + rolesSource, { registry });
    const Doc = models.Doc! as any, Draft = models.Draft! as any;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      await Doc.objects.using(db).insertMany([{ id: 1, title: "a" }, { id: 2, title: "b" }]);
      assert.equal(await Draft.objects.using(db).filter(Draft.id.eq(1)).update({ title: "x" }), 1);
      assert.equal(((await Doc.objects.using(db).get(Doc.id.eq(1))) as { version: number }).version, 1);
      assert.equal(await Draft.objects.using(db).filter(Draft.id.eq(2)).delete(), 1);
      assert.notEqual(((await Doc.objects.using(db).withoutDefaults("deleted").get(Doc.id.eq(2))) as { deleted: unknown }).deleted, null);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("a proxy must keep the soft-delete field", { skip: !hasExtension("monorm.proxy", "monorm.soft-delete") }, () => {
  const bad = rolesSource.replace("@@proxy.of(Doc)", '@@proxy.of(Doc)\n  @@proxy.fields(exclude: ["deleted"])');
  assert.throws(() => loads(bad, { registry: new Registry() }), /proxy must keep the soft-delete field deleted/);
});

const defaultsSource = `
model Member {
  id     Int     @id
  name   String
  status String
  bio    String?
  @@map("proxy_defaults_members_node")
}
model ActiveMember {
  status String @client_default("active")
  @@proxy.of(Member)
  @@query.defaults(filter: status == "active", fields: ["id", "name"])
}
`;
for (const dialect of ["sqlite", "postgres"]) {
  // case: proxy::proxy_with_query_defaults_is_a_filtered_view
  test(`a proxy with query defaults is a filtered view (${dialect})`, { skip: !hasExtension("monorm.proxy", "monorm.query-defaults") }, async () => {
    const registry = new Registry();
    const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + defaultsSource, { registry });
    const Member = models.Member! as any, Active = models.ActiveMember! as any;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.dropTables();
    await db.createTables();
    try {
      await Member.objects.using(db).insertMany([{ id: 1, name: "ann", status: "active", bio: "a" }, { id: 2, name: "bob", status: "old", bio: "b" }]);
      // the proxy policy filters and narrows; the parent model is unchanged
      const rows = await Active.objects.using(db).orderBy(Active.id).all();
      assert.deepEqual(rows.map((r: { constructor: { name: string } }) => r.constructor.name), ["ActiveMember"]);
      assert.deepEqual(rows[0].toJSON(), { id: 1, name: "ann" });
      assert.equal(rows.length, 1);
      assert.equal(rows[0].id, 1);
      assert.equal(rows[0].name, "ann");
      assert.throws(() => rows[0].bio, NotLoaded);
      assert.equal(await Active.objects.using(db).count(), 1);
      assert.equal(await Member.objects.using(db).count(), 2);
      assert.equal((await Member.objects.using(db).get(Member.id.eq(2)) as { bio: string }).bio, "b");
      assert.equal(await Active.objects.using(db).withoutDefaults().count(), 2);
      // an insert through the proxy uses its client default and is in its view
      await Active.objects.using(db).insert({ id: 3, name: "cat" });
      assert.deepEqual((await Active.objects.using(db).orderBy(Active.id).all()).map((r: { name: string }) => r.name), ["ann", "cat"]);
      // query set writes select through the policy filter
      assert.equal(await Active.objects.using(db).all().update({ bio: "x" }), 2);
      assert.equal((await Member.objects.using(db).get(Member.id.eq(2)) as { bio: string }).bio, "b");
      assert.equal(await Active.objects.using(db).all().delete(), 2);
      assert.deepEqual((await Member.objects.using(db).all()).map((m: { id: number }) => m.id), [2]);
    } finally { await db.dropTables(); await db.close(); }
  });
}
