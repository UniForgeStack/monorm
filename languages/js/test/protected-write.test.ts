/** `@@write_rules.protect(layer: app)` and `allowWrites`: an application-level write check. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { allowWrites, connect, loads, pull, Registry, SchemaError, WriteProtected, type Database } from "../src/index.js";
import { hasExtension } from "./artifact-caps.js";

const source = `datasource db {
  provider = "sqlite"
}
model Post {
  id    Int       @id
  title String
  tags  Tag[]     @relation(through: PostTag)
  links PostTag[]
  @@write_rules.protect(layer: app)
}
model Tag {
  id    Int       @id
  name  String
  posts Post[]    @relation(through: PostTag)
  links PostTag[]
}
model PostTag {
  id      Int  @id @default(autoincrement())
  post_id Int
  tag_id  Int
  post    Post @relation(fields: [post_id], references: [id])
  tag     Tag  @relation(fields: [tag_id], references: [id])
  @@unique([post_id, tag_id])
  @@write_rules.protect(layer: app)
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
async function blog(schema = source): Promise<{ models: Record<string, Any>; db: Database }> {
  const registry = new Registry();
  const models = loads(schema, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  return { models, db };
}

test("every write method fails outside allowWrites and works inside", async () => {
  const { models, db } = await blog();
  const Post = models["Post"];
  const posts = () => Post.objects.using(db);
  const writes: (() => Promise<unknown>)[] = [
    () => posts().insert({ id: 1, title: "a" }),
    () => posts().insertMany([{ id: 2, title: "b" }, { id: 3, title: "c" }]),
    () => posts().insert({ id: 1, title: "upsert" }).onConflict(Post.id, { update: true }),
    () => posts().filter(Post.id.eq(1)).update({ title: "x" }),
    () => posts().updateMany([{ id: 2, title: "y" }]),
    () => posts().filter(Post.id.eq(3)).delete(),
  ];
  try {
    for (const write of writes) {
      await assert.rejects(write(), (e: Error) => e instanceof WriteProtected && /Post is write-protected.*allowWrites\(\[Post\]/.test(e.message));
      await allowWrites([Post], write);
    }
    assert.deepEqual((await posts().orderBy(Post.id).all()).map((p: Any) => [p.id, p.title]), [[1, "x"], [2, "y"]]);
    const post = await posts().get(Post.id.eq(1));
    await assert.rejects(post.update({ title: "z" }), WriteProtected);
    await assert.rejects(post.delete(), WriteProtected);
    await allowWrites([Post], async () => { await post.update({ title: "z" }); await post.delete(); });
    assert.equal(await posts().count(), 1);
  } finally { await db.close(); }
});

test("the check is on the model the SQL writes", async () => {
  const { models, db } = await blog();
  const { Post, Tag, PostTag } = models;
  try {
    const post: Any = await allowWrites([Post], () => Post.objects.using(db).insert({ id: 1, title: "a" }));
    const tag = await Tag.objects.using(db).insert({ id: 1, name: "t" });
    await assert.rejects(allowWrites([Post], () => post.tags.add(tag)), (e: Error) => e instanceof WriteProtected && /PostTag/.test(e.message));
    await allowWrites([PostTag], () => post.tags.add(tag));
    assert.equal(await PostTag.objects.using(db).count(), 1);
    // Scopes nest: an inner scope adds to the models of the outer one.
    await allowWrites([Post], () => allowWrites([Tag], () => Post.objects.using(db).filter(Post.id.eq(1)).update({ title: "b" })));
    await assert.rejects(allowWrites(["Post"] as never, async () => {}), TypeError);
    await assert.rejects(allowWrites([post] as never, async () => {}), TypeError);
    await assert.rejects(allowWrites([] as never, undefined as never), TypeError);
    await assert.rejects(allowWrites(Post as never, async () => {}), /takes a list of models and a function/);
  } finally { await db.close(); }
});

// case: protected_write::a_package_check_before_its_own_io_also_checks_protection
test("a package check before its own I/O also checks protection", async () => {
  const { models, db } = await blog();
  const { Post } = models;
  try {
    // A package checks its write with prepareInsert/prepareUpdate before its own I/O (file uploads).
    assert.throws(() => Post.objects.using(db).prepareInsert({ id: 1, title: "a" }), (e: Error) => e instanceof WriteProtected && /Post/.test(e.message));
    assert.throws(() => Post.objects.using(db).filter(Post.id.eq(1)).prepareUpdate({ title: "b" }), (e: Error) => e instanceof WriteProtected && /Post/.test(e.message));
    await allowWrites([Post], async () => {
      Post.objects.using(db).prepareInsert({ id: 1, title: "a" });
      Post.objects.using(db).filter(Post.id.eq(1)).prepareUpdate({ title: "b" });
    });
  } finally { await db.close(); }
});

// case: protected_write::the_scope_follows_async_work_and_reads_and_raw_sql_are_unchanged
test("the scope follows async work and reads and raw SQL are unchanged", async () => {
  const { models, db } = await blog();
  const { Post } = models;
  const insert = async (id: number) => Post.objects.using(db).insert({ id, title: "a" });
  try {
    let task!: Promise<unknown>;
    await allowWrites([Post], async () => { task = insert(1); });
    await task;
    await assert.rejects(insert(2), WriteProtected);
    // A lazy write reads the scope at await, not where it was built.
    let lazy!: Promise<unknown>;
    await allowWrites([Post], async () => { lazy = Post.objects.using(db).insert({ id: 3, title: "a" }); });
    await assert.rejects(lazy, WriteProtected);
    assert.equal(await Post.objects.using(db).count(), 1);
    assert.equal(await db.execute("UPDATE post SET title = 'raw'"), 1);
    assert.equal((await Post.objects.using(db).get()).title, "raw");
  } finally { await db.close(); }
});

test("protected write creates no DDL change", () => {
  const schema = (text: string) => {
    const registry = new Registry();
    loads(text, { registry });
    const s = registry.native();
    return s.ddl();
  };
  assert.deepEqual(schema(source), schema(source.replaceAll("@@write_rules.protect(layer: app)", "")));
  assert.throws(() => loads(source.replace("@@write_rules.protect(layer: app)", "@@protected_write"), { registry: new Registry() }),
    (e: Error) => e instanceof SchemaError && /@@write_rules\.protect\(layer: app\)/.test(e.message));
});

test("a proxy writes the table of its protected root", { skip: !hasExtension("monorm.proxy") }, async () => {
  const { models, db } = await blog(source + "model Draft {\n  @@proxy.of(Post)\n}\n");
  const { Post, Draft } = models;
  try {
    await assert.rejects(Draft.objects.using(db).insert({ id: 1, title: "a" }), (e: Error) => e instanceof WriteProtected && /Post/.test(e.message));
    await allowWrites([Post], () => Draft.objects.using(db).insert({ id: 1, title: "a" }));
    await allowWrites([Draft], () => Draft.objects.using(db).filter(Draft.id.eq(1)).delete());
  } finally { await db.close(); }
});

test("a composed write also writes its protected parent", { skip: !hasExtension("monorm.composition") }, async () => {
  const { models, db } = await blog(`datasource db {
  provider = "sqlite"
}
model Person {
  id   Int    @id @default(autoincrement())
  name String
  @@write_rules.protect(layer: app)
}
model Employee {
  salary Int
  @@composition.model(parent: "Person", parentRef: "person", childRef: "employee")
}
`);
  const { Person, Employee } = models;
  try {
    await assert.rejects(Employee.objects.using(db).insert({ name: "a", salary: 1 }), (e: Error) => e instanceof WriteProtected && /Person/.test(e.message));
    const alice: Any = await allowWrites([Employee, Person], () => Employee.objects.using(db).insert({ name: "a", salary: 1 }));
    await assert.rejects(Employee.objects.using(db).filter(Employee.id.eq(alice.id)).update({ name: "b" }), WriteProtected);
    const bob: Any = await allowWrites([Person], () => Person.objects.using(db).insert({ name: "b" }));
    await assert.rejects(Employee.objects.using(db).attach(bob.id, { salary: 2 }), WriteProtected);
    await allowWrites([Employee, Person], () => Employee.objects.using(db).attach(bob.id, { salary: 2 }));
  } finally { await db.close(); }
});

const rules = `
model Ledger {
  id Int @id
  note String
  slug String @write_rules.read_only
  @@map("wr_js_ledger")
  @@write_rules.protect(operation: [delete])
  @@write_rules.protect(operation: [update], condition: "OLD.note = 'locked'", layer: db)
}
model Dropped {
  id Int @id
  @@map("wr_js_dropped_table_with_a_long_name_for_introspection")
  @@write_rules.protect(operation: [insert], layer: db, action: drop)
}
model Openable {
  id Int @id
  @@map("wr_js_openable")
  @@write_rules.protect(operation: [insert], skippable: true)
}
model Frozen {
  id Int @id
  @@map("wr_js_frozen")
  @@write_rules.protect
}
model Base {
  @@inherit.abstract
  @@write_rules.protect(operation: [update])
}
model Child {
  id Int @id
  note String
  @@inherit.extends("Base")
  @@map("wr_js_child")
}
`;

for (const provider of ["sqlite", "postgresql"]) {
  const postgres = provider === "postgresql";
  const url = postgres ? process.env["ORM_TEST_DATABASE_URL"] : "sqlite://:memory:";
  const scenario = (name: string, body: (db: Database, models: Record<string, Any>) => Promise<void>) =>
    test(name, { skip: !url }, async () => {
      const roles = postgres ? '\n  bypass_roles = ["postgres"]' : "";
      const registry = new Registry();
      const models = loads(`datasource db {\n  provider = "${provider}"${roles}\n}\n${rules}`, { registry }) as Record<string, Any>;
      const db = await connect(url!, { registry, default: false });
      await db.dropTables();
      await db.createTables();
      try {
        await body(db, models);
      } finally {
        await db.dropTables();
        await db.close();
      }
    });

  scenario("write rule layers agree on which writes they refuse", async (db, models) => {
    const { Ledger, Child, Frozen } = models;
    const rows = Ledger.objects.using(db);
    await rows.insertMany([{ id: 1, note: "open", slug: "a" }, { id: 2, note: "locked", slug: "b" }]);
    const row = rows.filter(Ledger.id.eq(1));
    await row.update({ note: "edited" });
    await assert.rejects(row.update({ slug: "a" }), (e: Error) => e instanceof WriteProtected && /Ledger/.test(e.message));
    assert.equal(await db.execute("UPDATE wr_js_ledger SET slug = 'a' WHERE id = 1"), 1);
    await assert.rejects(db.execute("UPDATE wr_js_ledger SET slug = 'new' WHERE id = 1"), (e: Error) => e instanceof WriteProtected && /read_only_slug/.test(e.message));
    await assert.rejects(rows.filter(Ledger.id.eq(2)).update({ note: "editable" }), WriteProtected);
    await assert.rejects(row.delete(), WriteProtected);
    await assert.rejects(allowWrites([Ledger], () => row.delete()), WriteProtected);
    await assert.rejects(allowWrites([Frozen], () => Frozen.objects.using(db).insert({ id: 1 })), WriteProtected);
    assert.equal(await db.execute("INSERT INTO wr_js_dropped_table_with_a_long_name_for_introspection (id) VALUES (1)"), 0);
    await Child.objects.using(db).insert({ id: 1, note: "a" });
    await assert.rejects(Child.objects.using(db).filter(Child.id.eq(1)).update({ note: "b" }), (e: Error) => e instanceof WriteProtected && /Child/.test(e.message));
  });

  scenario("a child of a protected base refuses a raw update in its own table", async (db, models) => {
    const { Child } = models;
    await Child.objects.using(db).insertMany([{ id: 1, note: "a" }, { id: 2, note: "b" }]);
    await assert.rejects(db.execute("UPDATE wr_js_child SET note = 'raw' WHERE id = 1"), (e: Error) => e instanceof WriteProtected && /protect_/.test(e.message));
    assert.equal((await Child.objects.using(db).filter(Child.id.eq(1)).first()).note, "a");
    // The inherited rule names the update only, so the database still takes a raw delete.
    assert.equal(await db.execute("DELETE FROM wr_js_child WHERE id = 2"), 1);
  });

  // case: protected_write::allow_writes_opens_only_skippable_rules_for_approved_roles
  scenario("allowWrites opens only skippable rules for approved roles", async (db, models) => {
    const { Openable } = models;
    const rows = Openable.objects.using(db);
    await assert.rejects(rows.insert({ id: 1 }), (e: Error) => e instanceof WriteProtected && /Openable/.test(e.message));
    await allowWrites([Openable], async () => {
      await rows.insert({ id: 1 });
      await db.transaction(() => rows.insert({ id: 2 }).prepare().run());
    });
    if (!postgres) return;
    await assert.rejects(db.execute("INSERT INTO wr_js_openable (id) VALUES (3)"), WriteProtected);
    assert.equal(await allowWrites([Openable], () => db.execute("INSERT INTO wr_js_openable (id) VALUES (3)")), 1);
    await assert.rejects(allowWrites([Openable], () => db.transaction(async () => {
      await db.execute("CREATE ROLE monorm_wr_js_scoped_plain");
      await db.execute("GRANT INSERT, SELECT ON wr_js_openable TO monorm_wr_js_scoped_plain");
      await db.execute("SET LOCAL SESSION AUTHORIZATION monorm_wr_js_scoped_plain");
      await rows.insert({ id: 4 });
    })), (e: Error) => e instanceof WriteProtected && /monorm write rule/.test(e.message));
    assert.equal(await rows.count(), 3);
  });
  scenario("pull recovers rule attributes", async (db) => {
    const pulled = await pull(db);
    assert.ok(pulled.schema.includes("@write_rules.read_only"));
    assert.ok(pulled.schema.includes("@@write_rules.protect(operation: [update], layer: db, condition:"));
    assert.ok(pulled.schema.includes("@@write_rules.protect(operation: [insert], layer: db, action: drop)"));
    assert.deepEqual(pulled.gaps.filter((gap) => /wr_js_|write rule claims/.test(gap)), []);
  });

}
