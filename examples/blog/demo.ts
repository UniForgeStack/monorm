/**
 * A tour of the TypeScript API, the counterpart of demo.py, on SQLite or PostgreSQL.
 * Build the `monorm` package first (`cd languages/js && npm install && npm run build:native && npm run build`),
 * then from this directory:
 *
 *     npm install
 *     node demo.ts sqlite://:memory:          (Node 23.6+; or: npm run demo -- <url>, bun demo.ts <url>)
 *     node demo.ts postgres://postgres:postgres@localhost/orm_test
 *
 * The core section runs on every build. Each extension section runs when the native addon
 * has that extension, and prints how to get it otherwise. `scripts/install-extension-packages.sh` installs the extension packages into languages/js.
 *
 * The file-storage section also needs the `@ufstack/monorm-file-storage` package (`scripts/install-extension-packages.sh` installs it into languages/js). Each section creates its demo_* tables and drops them.
 *
 * The demo loads its schemas at run time (`loads`), so its models are untyped. Generated
 * models (`monorm generate typescript`, see examples/blog/models.ts) type every column and row.
 */

import { createRequire } from "node:module";
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { allowWrites, connect, debug, exists, func, load, loads, NotLoaded, outer, param, Registry, SchemaError, WriteProtected, type Database } from "@ufstack/monorm";

type Models = Record<string, any>;

const HERE = join(import.meta.dirname, "demo");

function schema(name: string, provider: string): string {
  return `datasource db {\n  provider = "${provider}"\n}\n` + readFileSync(join(HERE, `${name}.prisma`), "utf8");
}

/** Loads the section's schema into its own registry, runs it, and drops its tables. */
async function section(name: string, url: string, run: (db: Database, m: Models) => Promise<void>,
                       loader?: (provider: string, registry: Registry) => Models): Promise<void> {
  console.log(`\n== ${name}`);
  const provider = url.startsWith("sqlite:") ? "sqlite" : "postgresql";
  const registry = new Registry();
  let models: Models;
  try {
    models = loader ? loader(provider, registry) : loads(schema(name, provider), { registry });
  } catch (e) {
    // An extension attribute, or the extension type `Generic`, on a build without it.
    if (!(e instanceof SchemaError) || !/not compiled into this artifact|extension/.test(e.message)) throw e;
    console.log(`skipped: this native addon has no ${name} extension (${e.message})`);
    return;
  }
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    await run(db, models);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

async function core(db: Database, m: Models): Promise<void> {
  const { User, Post, Comment, Tag, AuditLog } = m;

  // Writes are explicit statements: INSERT ... RETURNING gives back the stored rows.
  // `token` has @client_default(uuid()): the ORM fills it, on SQLite too.
  const alice = await User.objects.using(db).insert({ email: "alice@example.com", name: "Alice", role: "admin" });
  let bob = await User.objects.using(db).insert({ email: "bob@example.com", name: "Bob" });
  console.log("client default token:", alice.token.length === 36);
  const posts = await Post.objects.using(db).insertMany(
    [0, 1, 2, 3, 4, 5].map((i) => ({ author: alice, title: `Post ${i}`, views: i, published: i % 2 === 0 })),
  ).returning();
  await posts[0].comments.insert({ body: "first!", author: bob });
  // Upsert on the unique email.
  bob = await User.objects.using(db).insert({ email: "bob@example.com", name: "Robert" }).onConflict(User.email, { update: true }).returning();
  console.log("upserted:", bob.name);

  // Filters follow relations; a to-many hop is an EXISTS, so no duplicate rows.
  console.log("authors with a published post:", (await User.objects.using(db).filter(User.posts.published)).map((u: Models) => u.name));
  // Eager loading: JOIN for to-one, one IN query for to-many; "-name" orders descending.
  const c = await Comment.objects.using(db).load(Comment.post.author, Comment.author).require().get();
  console.log(`${c.author.name} on ${c.post.author.name}'s ${JSON.stringify(c.post.title)}`);
  for (const u of await User.objects.using(db).load(User.posts).orderBy("-name")) {
    console.log(u.name, u.posts.cached.map((p: Models) => p.title));
  }
  // Explicit loaders for one reference (the reference-loading feature, on by default).
  await c.load(Comment.post);
  console.log("loaded:", c.post.title);

  // select(): object rows of columns, aggregates, string functions and correlated subqueries.
  const rows = await User.objects.using(db).select({
    who: func.concat(User.name, " <", User.email, ">"),
    posts: func.count(User.posts),
    popular: exists(Post.objects.filter(Post.authorId.eq(outer(User.id)), Post.views.gt(4))),
  }).orderBy(User.name);
  console.log(rows);

  // Many-to-many through PostTag.
  await posts[0].tags.add(await Tag.objects.using(db).insert({ name: "rust" }));
  console.log("tagged rust:", await Post.objects.using(db).filter(Post.tags.name.eq("rust")).count());

  // Prepared queries: build once, bind values per call.
  const byAuthor = Post.objects.using(db).filter(Post.authorId.eq(param("author"))).orderBy(Post.id).limit(param("n")).prepare();
  console.log("prepared:", (await byAuthor.all({ author: alice.id, n: 2 })).map((p: Models) => p.title));

  // Cursor pagination: keyset pages with an opaque cursor.
  const qs = Post.objects.using(db).orderBy("-views");
  let page = await qs.paginate({ first: 4 });
  console.log("page 1:", page.items.map((p: Models) => p.views), "has next:", page.hasNext);
  page = await qs.paginate({ first: 4, after: page.nextCursor });
  console.log("page 2:", page.items.map((p: Models) => p.views), "has next:", page.hasNext);

  // Set-based UPDATE, and a single-row update that refreshes the instance.
  await Post.objects.using(db).filter(Post.authorId.eq(alice.id)).update({ views: Post.views.add(1) });
  await db.transaction(() => posts[0].update({ title: "Hello, world", views: Post.views.add(10) }));
  console.log("views after update:", posts[0].views); // 11: the database's value, read back with RETURNING

  // Protected writes: an application-level check, not a database permission.
  try {
    await AuditLog.objects.using(db).insert({ action: "outside" });
  } catch (e) {
    if (!(e instanceof WriteProtected)) throw e;
    console.log("write protected:", e.message);
  }
  await allowWrites([AuditLog], () => AuditLog.objects.using(db).insert({ action: "inside" }));
  console.log("audit rows:", await AuditLog.objects.using(db).count());

  // The N+1 finder: one load per row in a loop is reported with its fix.
  const added = await Comment.objects.using(db).insertMany(posts.map((p: Models) => ({ postId: p.id, body: "+1" }))).returning();
  const comments = await Comment.objects.using(db).filter(Comment.id.in(added.map((x: Models) => x.id)));
  try {
    await debug.nPlusOne(async () => { for (const x of comments) await x.load(Comment.post); }, { threshold: 3, fail: true });
  } catch (e) {
    if (!(e instanceof debug.NPlusOne)) throw e;
    console.log("N+1:", e.report.repeated.map((s) => [s.count, s.fix]));
  }
}

async function proxyModels(db: Database, m: Models): Promise<void> {
  const { Account, ActiveAccount } = m;
  await Account.objects.using(db).insertMany([
    { id: 1, name: "old", status: "old", note: "x" },
    { id: 2, name: "live", status: "active" },
  ]);
  // The proxy fills its client defaults and has no `note` field.
  const created = await ActiveAccount.objects.using(db).insert({ id: 3 });
  console.log("proxy insert:", created.name, created.status, "note" in created);
  console.log("rows through the proxy:", await ActiveAccount.objects.using(db).count(), "(one shared table)");
}

async function queryDefaults(db: Database, m: Models): Promise<void> {
  const { Article } = m;
  await Article.objects.using(db).insertMany([
    { title: "a", body: "long" }, { title: "b", body: "long" }, { title: "hidden", body: "x", visible: false },
  ]);
  const rows = await Article.objects.using(db).all();
  console.log("default filter and order:", rows.map((r: Models) => r.title));
  try {
    rows[0].body;
  } catch (e) {
    if (!(e instanceof NotLoaded)) throw e;
    console.log("body is selected out by default");
  }
  console.log("withoutDefaults():", await Article.objects.using(db).withoutDefaults().count(), "rows");
}

async function modelComposition(db: Database, m: Models): Promise<void> {
  const { Person, Employee, Customer } = m;
  const ann = await Employee.objects.using(db).insert({ name: "Ann", salary: 10 }); // a Person row and an Employee row
  await Customer.objects.using(db).attach(ann.id, { points: 5 }); // Ann is also a customer
  await Employee.objects.using(db).filter(Employee.name.eq("Ann")).update({ salary: 12 });
  const person = await Person.objects.using(db).load(Person.employee, Person.customer).require().get();
  console.log("composed:", person.name, person.employee.salary, person.customer.points);
}

async function fileStorage(db: Database, m: Models): Promise<void> {
  // Imported by path: the storage package and opendal are not dependencies of this example.
  const root = new URL("../../languages/js/node_modules/@ufstack/monorm-file-storage/", import.meta.url);
  let files: Models, model: Models, opendal: Models;
  try {
    [files, model] = await Promise.all(["index.js", "model.js"].map((path) => import(new URL(path, root).href)));
    opendal = createRequire(new URL("../../languages/js/package.json", import.meta.url))("opendal");
  } catch {
    console.log("skipped: install @ufstack/monorm-file-storage with scripts/install-extension-packages.sh");
    return;
  }
  const { Report } = m;
  const directory = realpathSync(mkdtempSync(join(tmpdir(), "monorm-demo-"))); // the fs service wants a canonical root
  try {
    const providers = new files.Registry(new Map([["reports", new opendal.Operator("fs", { root: directory })]]));
    model.installModel(Report._meta.Row.prototype, new Map([["file", new files.FileField("file", "reports", true)]]), providers, Report._meta);
    await Report.objects.using(db).insert({ id: 1, file: new files.Upload(Buffer.from("quarterly numbers"), { filename: "q3.txt" }) });
    const report = await Report.objects.using(db).require().get(Report.id.eq(1));
    const chunks: Buffer[] = [];
    for await (const chunk of report.fileOpen()) chunks.push(Buffer.from(chunk));
    console.log("file field:", report.file.constructor.name, Buffer.concat(chunks).toString());
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

function loadGeneric(provider: string, registry: Registry): Models {
  // Identities live next to the schema; the demo copies both to a scratch directory.
  const directory = mkdtempSync(join(tmpdir(), "monorm-demo-"));
  try {
    const path = join(directory, "schema.prisma");
    writeFileSync(path, schema("generic-relations", provider));
    writeFileSync(join(directory, "schema.identities.json"), readFileSync(join(HERE, "generic-relations.identities.json"), "utf8"));
    // Enums are values of the registry: ContentType holds the frozen model IDs.
    return { ...load(path, { registry }), ContentType: registry.getEnum("ContentType") };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

async function genericRelations(db: Database, m: Models): Promise<void> {
  const { Photo, Video, Label, ContentType } = m;
  const photo = await Photo.objects.using(db).insert({ id: 1, url: "cat.png" });
  await Video.objects.using(db).insert({ id: 1, title: "cats" });
  await Label.objects.using(db).insertMany([
    { id: 1, name: "cute", targetType: ContentType.Photo, targetId: photo.id },
    { id: 2, name: "long", targetType: ContentType.Video, targetId: 1 },
  ]);
  const onPhotos = await Label.objects.using(db).filter(Label.targetType.eq(ContentType.Photo));
  console.log("labels on photos:", onPhotos.map((x: Models) => x.name), "ContentType.Photo =", ContentType.Photo);
}

async function main(url: string): Promise<void> {
  await section("core", url, core);
  await section("proxy-models", url, proxyModels);
  await section("query-defaults", url, queryDefaults);
  await section("model-composition", url, modelComposition);
  await section("file-storage", url, fileStorage);
  await section("generic-relations", url, genericRelations, loadGeneric);
}

await main(process.argv[2] ?? "sqlite://:memory:");
