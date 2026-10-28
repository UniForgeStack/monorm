/** Extension packages: the npm discovery through `"monorm": {"extension": ...}` in the direct dependencies. */
import { createHash } from "node:crypto";
import ts from "typescript";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test, type TestContext } from "node:test";
import { connect, define, IntegrityError, load, loads, QueryError, Registry, SchemaError, scope, type SchemaIR } from "../src/index.js";
import { native } from "../src/native.js";
import { resolveExtensions } from "../src/packages.js";

// `npx @ufstack/monorm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;
const MANIFEST = 'id = "acme.fixture"\nversion = "0.1.0"\nabi = "0.2"\nlibrary = "libfixture"\n';

function write(file: string, text: string): string {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, text);
  return file;
}

function directory(t: TestContext): string {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "monorm-extensions-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** An installed package `name` under `root/node_modules`, with the extension id and manifest unless null. */
function pkg(root: string, name: string, id: string | null, manifest: boolean = true): string {
  const dir = join(root, "node_modules", name);
  write(join(dir, "package.json"), JSON.stringify({ name, version: "1.0.0", ...(id ? { monorm: { extension: id } } : {}) }));
  if (id === "acme.outside") {
    // The package imports `orm`, so the project needs it installed.
    if (!existsSync(join(root, "node_modules", "orm"))) symlinkSync(process.cwd(), join(root, "node_modules", "orm"), "dir");
    const source = readFileSync(join(OUTSIDE, "typescript/index.ts"), "utf8");
    write(join(dir, "index.ts"), source);
    write(join(dir, "index.cjs"), ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText);
    write(join(dir, "package.json"), JSON.stringify({ name, version: "1.0.0", main: "index.cjs", types: "index.ts", monorm: { extension: id } }));
  }
  return manifest ? write(join(dir, "monorm-extension.toml"), MANIFEST) : dir;
}

function project(root: string, dependencies: Record<string, string>, key = "dependencies"): void {
  write(join(root, "package.json"), JSON.stringify({ name: "app", [key]: dependencies }));
}

test("a direct dependency with an extension id resolves to its manifest", (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-fixture", "acme.fixture");
  pkg(root, "left-pad", null);
  project(root, { "acme-fixture": "1.0.0", "left-pad": "1.0.0" });
  assert.deepEqual(resolveExtensions(join(root, "schema.prisma")), { "acme.fixture": manifest });
});

test("devDependencies and optionalDependencies count, and a subdirectory finds the project", (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-fixture", "acme.fixture");
  project(root, { "acme-fixture": "1.0.0" }, "devDependencies");
  assert.deepEqual(resolveExtensions(join(root, "src", "deep", "schema.prisma")), { "acme.fixture": manifest });
  project(root, { "acme-fixture": "1.0.0" }, "optionalDependencies");
  assert.deepEqual(resolveExtensions(join(root, "schema.prisma")), { "acme.fixture": manifest });
});

test("without a schema file the project is the one of the working directory", (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-fixture", "acme.fixture");
  project(root, { "acme-fixture": "1.0.0" });
  const cwd = process.cwd();
  process.chdir(root);
  t.after(() => process.chdir(cwd));
  assert.deepEqual(resolveExtensions(), { "acme.fixture": manifest });
});

test("a package that only another package depends on is not an extension of the project", (t) => {
  const root = directory(t);
  pkg(root, "acme-fixture", "acme.fixture");
  project(root, {});
  assert.deepEqual(resolveExtensions(join(root, "schema.prisma")), {});
});

test("no package.json means no extensions", (t) => {
  assert.deepEqual(resolveExtensions(join(directory(t), "schema.prisma")), {});
});

test("a package without a manifest file names the package", (t) => {
  const root = directory(t);
  pkg(root, "acme-fixture", "acme.fixture", false);
  project(root, { "acme-fixture": "1.0.0" });
  assert.throws(() => resolveExtensions(join(root, "schema.prisma")), (e: unknown) => e instanceof SchemaError && /acme-fixture.*monorm-extension\.toml/.test(e.message));
});

test("two packages with one id name both paths", (t) => {
  const root = directory(t);
  const first = pkg(root, "acme-one", "acme.fixture");
  const second = pkg(root, "acme-two", "acme.fixture");
  project(root, { "acme-one": "1.0.0", "acme-two": "1.0.0" });
  assert.throws(() => resolveExtensions(join(root, "schema.prisma")), (e: unknown) => e instanceof SchemaError && e.message.includes("installed twice") && e.message.includes(first) && e.message.includes(second));
});

const OUTSIDE = resolve(process.cwd(), "../../tests/outside-extension");

/** The library of `tests/outside-extension`, built in its own workspace. */
function outsideLibrary(t: TestContext): string {
  const target = realpathSync(mkdtempSync(join(tmpdir(), "monorm-outside-")));
  t.after(() => rmSync(target, { recursive: true, force: true }));
  execFileSync("cargo", ["build", "--offline", "--quiet", "--manifest-path", join(OUTSIDE, "Cargo.toml"), "--target-dir", target], { stdio: "inherit" });
  const file = readdirSync(join(target, "debug")).find((f) => /^liboutside_extension\.(dylib|so|dll)$/.test(f));
  assert.ok(file, "the outside library is built");
  return join(target, "debug", file);
}

test("an installed outside library lowers the schema through load(), loads() and the command line", { timeout: 300_000 }, async (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-outside", "acme.outside", false) + "/orm-extension.toml";
  write(manifest, readFileSync(join(OUTSIDE, "monorm-extension.toml"), "utf8"));
  const library = outsideLibrary(t);
  copyFileSync(library, join(dirname(manifest), library.slice(library.lastIndexOf("/") + 1)));
  project(root, { "acme-outside": "1.0.0" });
  const schema = write(join(root, "schema.prisma"), 'datasource db { provider = "sqlite" }\nmodel Post {\n id Int @id\n @@outside.tag("blue")\n}\n');
  const { Post } = load(schema, { registry: new Registry() }) as Record<string, any>;
  assert.ok(Post?._meta.fields.has("tag"), "the pass adds the field");
  assert.match(Post.objects.tagged().sql(), /LIMIT 7/);
  const db = await connect("sqlite://:memory:", { default: false, registry: Post._meta.registry });
  try {
    await db.createTables();
    const row = await Post.objects.using(db).insert({ id: 1, tag: "blue" });
    assert.equal(row.tagLabel(), "blue");
  } finally { await db.close(); }

  // `loads()` has no file, so the extensions come from the working directory.
  const cwd = process.cwd();
  process.chdir(root);
  t.after(() => process.chdir(cwd));
  const loaded = loads('datasource db { provider = "sqlite" }\nmodel Post {\n id Int @id\n @@outside.tag("blue")\n}\n', { registry: new Registry() });
  assert.ok(loaded.Post?._meta.fields.has("tag"), "loads() finds the library from the working directory");
  assert.match((loaded.Post as any).objects.tagged().sql(), /LIMIT 7/);

  // The command line resolves the extensions from the schema file, in a process of its own.
  const run = (...args: string[]) => {
    const done = spawnSync(process.execPath, [CLI, ...args], { cwd: root, encoding: "utf8", env: { ...process.env, MONORM_DATABASE_URL: "" } });
    assert.equal(done.status, 0, done.stderr);
    return done.stdout;
  };
  run("check");
  run("makemigrations");
  assert.ok(run("sqlmigrate", "1").includes('"tag"'), "the migration adds the column of the pass");
  const runtime = new URL(`../src/index.${CLI.endsWith(".ts") ? "ts" : "js"}`, import.meta.url).href;
  run("generate", "--import", runtime);
  // Compiled static extensions can write sources here; packaged extensions cannot.
  assert.equal(existsSync(join(root, "monorm-ext", "acme-outside")), false);
  const generated = readFileSync(join(root, "models.ts"), "utf8");
  write(join(root, "models.mjs"), ts.transpileModule(generated, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText);
  const embedded = JSON.parse(generated.match(/const SCHEMA: SchemaIR = ([\s\S]+?);\n/)![1]!) as SchemaIR;
  const stamped = embedded.behavior as { packages: Record<string, { languages: string[]; databases: string[] }> };
  stamped.packages["acme.outside"]!.languages = ["python"];
  assert.throws(() => define(embedded, { registry: new Registry() }), /does not support this language binding/);
  stamped.packages["acme.outside"]!.languages = ["typescript"];
  stamped.packages["acme.outside"]!.databases = ["postgres"];
  assert.throws(() => define(embedded, { registry: new Registry() }), /does not support sqlite/);
  const execute = () => spawnSync(process.execPath, ["--input-type=module", "-e", `import { Post } from './models.mjs'; if (!Post.objects.tagged().sql().includes('LIMIT 7')) throw Error('missing runtime method');`], { cwd: root, encoding: "utf8", env: process.env });
  const equal = execute();
  assert.equal(equal.status, 0, equal.stderr);
  assert.doesNotMatch(equal.stderr, /OrmStaleModelsWarning/);
  const runtimeFile = join(dirname(manifest), "index.cjs");
  const originalRuntime = readFileSync(runtimeFile, "utf8");
  const hash = createHash("sha256").update(readFileSync(manifest)).digest("hex");
  assert.ok(originalRuntime.includes(hash), "the hand-written runtime constant hashes the raw manifest");
  // Starting generated models must work even with no pass library installed.
  const installedLibrary = join(dirname(manifest), library.slice(library.lastIndexOf("/") + 1));
  rmSync(installedLibrary);
  for (const code of [originalRuntime.replace(hash, "changed"), originalRuntime.replace(/exports\.manifestHash = "[a-f0-9]+";/, "delete exports.manifestHash;")]) {
    writeFileSync(runtimeFile, code);
    const staleRuntime = execute();
    assert.equal(staleRuntime.status, 0, staleRuntime.stderr);
    assert.match(staleRuntime.stderr, /OrmStaleModelsWarning.*acme.outside/);
    assert.match(staleRuntime.stderr, /run `monorm generate`/);
  }
  writeFileSync(runtimeFile, originalRuntime);
  copyFileSync(library, installedLibrary);
  run("check");
  const original = readFileSync(manifest, "utf8");
  writeFileSync(manifest, original + "\n# changed after generation\n");
  const stale = spawnSync(process.execPath, [CLI, "check"], { cwd: root, encoding: "utf8" });
  assert.equal(stale.status, 1, stale.stderr);
  assert.match(stale.stderr, /acme.outside/);
  assert.match(stale.stderr, /run `monorm generate`/);
  writeFileSync(manifest, original);
  run("check");

  writeFileSync(manifest, original.replace('languages = ["python", "typescript"]', 'languages = ["python"]'));
  assert.throws(() => load(schema, { registry: new Registry() }), /does not support this language binding/);
  writeFileSync(manifest, original);

  // Even a package with no runtime classes is imported for its manifest hash.
  writeFileSync(manifest, original.split("\n[python]")[0]!);
  const hashOnly = createHash("sha256").update(readFileSync(manifest)).digest("hex");
  writeFileSync(runtimeFile, `exports.manifestHash = "${hashOnly}";\n`);
  run("generate", "--import", runtime);
  write(join(root, "models.mjs"), ts.transpileModule(readFileSync(join(root, "models.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText);
  rmSync(installedLibrary);
  const noClasses = spawnSync(process.execPath, ["--input-type=module", "-e", `import { Post } from './models.mjs'; if (typeof Post.objects.tagged !== 'undefined') throw Error('unexpected mixin');`], { cwd: root, encoding: "utf8", env: process.env });
  assert.equal(noClasses.status, 0, noClasses.stderr);
  assert.doesNotMatch(noClasses.stderr, /OrmStaleModelsWarning/);
});

test("an outside pass writes a two-table model that inserts, reads, updates and attaches", { timeout: 300_000 }, async (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-outside", "acme.outside", false) + "/orm-extension.toml";
  write(manifest, readFileSync(join(OUTSIDE, "monorm-extension.toml"), "utf8"));
  const library = outsideLibrary(t);
  copyFileSync(library, join(dirname(manifest), library.slice(library.lastIndexOf("/") + 1)));
  project(root, { "acme-outside": "1.0.0" });
  const schema = write(
    join(root, "schema.prisma"),
    'datasource db { provider = "sqlite" }\nmodel Account {\n id Int @id @default(autoincrement())\n email String\n}\nmodel Profile {\n bio String\n @@outside.base(parent: "Account", parentRef: "account", childRef: "profile")\n}\n',
  );
  const { Account, Profile } = load(schema, { registry: new Registry() }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { default: false, registry: Profile._meta.registry });
  try {
    await db.createTables();
    const ada = await Profile.objects.using(db).insert({ email: "ada@example.com", bio: "maths" });
    assert.deepEqual([ada.email, ada.bio], ["ada@example.com", "maths"]);
    assert.equal(await Account.objects.using(db).count(), 1);
    await Profile.objects.using(db).filter(Profile.email.eq("ada@example.com")).update({ bio: "engines" });
    assert.equal((await Profile.objects.using(db).filter(Profile.id.eq(ada.id)).get()).bio, "engines");
    const bob = await Account.objects.using(db).insert({ email: "bob@example.com" });
    const attached = await Profile.objects.using(db).attach(bob.id, { bio: "chess" });
    assert.deepEqual([attached.id, attached.email, attached.bio], [bob.id, "bob@example.com", "chess"]);
    await assert.rejects(Profile.objects.using(db).attach(bob.id, { bio: "again" }), IntegrityError);
  } finally { await db.close(); }
});

const GUARDED = 'datasource db { provider = "sqlite" }\nmodel Note {\n id Int @id\n text String\n start Int\n end Int\n shop_id Int\n updated_by String?\n @@outside.guard\n}\n';

test("hooks of an outside library: one error prefix, a caller value for a hook 4 field, and the unbound native schema", { timeout: 300_000 }, async (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-outside", "acme.outside", false) + "/orm-extension.toml";
  write(manifest, readFileSync(join(OUTSIDE, "monorm-extension.toml"), "utf8"));
  const library = outsideLibrary(t);
  copyFileSync(library, join(dirname(manifest), library.slice(library.lastIndexOf("/") + 1)));
  project(root, { "acme-outside": "1.0.0" });
  const schema = write(join(root, "schema.prisma"), GUARDED);
  const { Note } = load(schema, { registry: new Registry() }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { default: false, registry: Note._meta.registry });
  try {
    await db.createTables();
    const notes = Note.objects.using(db);
    await scope({ shop: 1, user: "ann" }, async () => {
      const failure = async (row: Record<string, unknown>) => {
        try { await notes.insert(row); } catch (error) { return error as Error; }
        return assert.fail("expected a hook error");
      };
      const empty = await failure({ id: 1, text: "  ", start: 1, end: 2, shopId: 1 });
      const ordered = await failure({ id: 1, text: "x", start: 3, end: 1, shopId: 1 });
      assert.ok(empty instanceof QueryError && ordered instanceof QueryError);
      assert.ok(empty.message.endsWith("Note.text: must not be empty"), empty.message);
      assert.ok(ordered.message.endsWith("Note: start 3 must be before end 1"), ordered.message);
      assert.doesNotMatch(`${empty.message}${ordered.message}`, /extension/);
      // An insert keeps the value that the caller supplies; the hook assigns on update only.
      await notes.insertMany([1, 2, 3].map((id) => ({ id, text: "a", start: 1, end: 2, shopId: 1, updatedBy: "mallory" })));
      assert.deepEqual((await notes.all()).map((n: any) => n.updatedBy), ["mallory", "mallory", "mallory"]);
      await (await notes.get(Note.id.eq(1))).update({ updatedBy: "eve" });
      assert.equal((await notes.get(Note.id.eq(1))).updatedBy, "ann");
      assert.equal(await notes.filter(Note.id.gt(1)).update({ updatedBy: "eve" }), 2);
      assert.deepEqual((await notes.all()).map((n: any) => n.updatedBy), ["ann", "ann", "ann"]);
    });
    await scope({ shop: 1 }, async () => {
      await assert.rejects(notes.filter(Note.id.eq(1)).update({ updatedBy: "eve" }), /no user in the request scope/);
    });
  } finally { await db.close(); }

  // Migrations and code generation bind no hook, so the native schema needs no package directory; `define()` passes them.
  const ir = native().compileSchemaFile(schema, {}, resolveExtensions(schema));
  const Schema = native().Schema;
  assert.doesNotThrow(() => new Schema(ir));
  assert.throws(() => new Schema(ir, {}), /acme\.outside: the schema binds its hooks, but no package directory was given/);
});

test("ESM-only and dual packages share runtime identity across generated models, load and loads", { timeout: 300_000 }, async (t) => {
  const library = outsideLibrary(t);
  for (const format of ["ESM-only", "dual", "module-sync"]) {
    const root = directory(t);
    const manifest = pkg(root, "acme-outside", "acme.outside", false) + "/orm-extension.toml";
    const dir = dirname(manifest);
    write(manifest, readFileSync(join(OUTSIDE, "monorm-extension.toml"), "utf8"));
    copyFileSync(library, join(dir, library.slice(library.lastIndexOf("/") + 1)));
    const source = readFileSync(join(OUTSIDE, "typescript/index.ts"), "utf8").replace(
      "tagged() {", "runtimeIdentity() { return OutsideQuerySet; } tagged() {",
    );
    write(join(dir, "index.mjs"), ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText);
    // Bun cannot `require` the ESM `orm`, and this test never installs an adapter.
    const withoutOrm = source.replace(/^import \{ registerFieldAdapter.*$/m, 'import type { FieldAdapter, ModelMeta } from "@ufstack/monorm"; const registerFieldAdapter = (): never => { throw Error("not installed"); };');
    write(join(dir, "index.cjs"), ts.transpileModule(withoutOrm, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText);
    write(join(dir, "package.json"), JSON.stringify({ name: "acme-outside", type: "module", monorm: { extension: "acme.outside" },
      exports: format === "ESM-only" ? "./index.mjs" : format === "dual"
        ? { import: "./index.mjs", require: "./index.cjs" }
        : { "module-sync": "./index.mjs", default: "./index.cjs" } }));
    project(root, { "acme-outside": "1.0.0" });
    const sourceSchema = 'datasource db { provider = "sqlite" }\nmodel Post {\n id Int @id\n @@outside.tag("blue")\n}\n';
    write(join(root, "schema.prisma"), sourceSchema);
    const runtime = new URL(`../src/index.${CLI.endsWith(".ts") ? "ts" : "js"}`, import.meta.url).href;
    const generated = spawnSync(process.execPath, [CLI, "generate", "--import", runtime], { cwd: root, encoding: "utf8", env: process.env });
    assert.equal(generated.status, 0, generated.stderr);
    write(join(root, "models.mjs"), ts.transpileModule(readFileSync(join(root, "models.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText);
    const execute = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import assert from 'node:assert/strict';
      import { Post } from './models.mjs';
      import { OutsideQuerySet } from 'acme-outside';
      import { load, loads, Registry } from ${JSON.stringify(runtime)};
      for (const Model of [Post, load('schema.prisma', { registry: new Registry() }).Post,
        loads(${JSON.stringify(sourceSchema)}, { registry: new Registry() }).Post]) {
        assert.equal(Model.objects.runtimeIdentity(), OutsideQuerySet);
        assert.match(Model.objects.tagged().sql(), /LIMIT 7/);
      }
    `], { cwd: root, encoding: "utf8", env: process.env });
    assert.equal(execute.status, 0, `${format}: ${execute.stderr}`);
    assert.doesNotMatch(execute.stderr, /OrmStaleModelsWarning/);
  }
});

test("an outside view reads and writes through the row filter and warns with its own code", { timeout: 300_000 }, (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-outside", "acme.outside", false) + "/orm-extension.toml";
  write(manifest, readFileSync(join(OUTSIDE, "monorm-extension.toml"), "utf8"));
  const library = outsideLibrary(t);
  copyFileSync(library, join(dirname(manifest), library.slice(library.lastIndexOf("/") + 1)));
  project(root, { "acme-outside": "1.0.0" });
  const source = 'datasource db { provider = "sqlite" }\nmodel Ticket {\n id Int @id\n status String\n note String?\n}\n'
    + 'model OpenTicket {\n id Int @id\n status String\n note String\n @@outside.view(Ticket, "open")\n}\n';
  const runtime = new URL(`../src/index.${CLI.endsWith(".ts") ? "ts" : "js"}`, import.meta.url).href;
  // The warning goes to the stderr of the native layer, so the program runs in a process of its own.
  const done = spawnSync(process.execPath, ["--input-type=module", "-e", `
    import assert from 'node:assert/strict';
    import { connect, loads, Registry } from ${JSON.stringify(runtime)};
    const registry = new Registry();
    const { Ticket, OpenTicket } = loads(${JSON.stringify(source)}, { registry });
    const db = await connect('sqlite://:memory:', { default: false, registry });
    await db.createTables();
    await Ticket.objects.using(db).insertMany([{ id: 1, status: 'open', note: null }, { id: 2, status: 'closed', note: 'kept' }, { id: 3, status: 'open', note: 'seen' }]);
    const rows = await OpenTicket.objects.using(db).all();
    assert.deepEqual(rows.map((r) => r.id).sort(), [1, 3]);
    assert.equal(await OpenTicket.objects.using(db).filter(OpenTicket.id.eq(2)).count(), 0);
    await OpenTicket.objects.using(db).insert({ id: 4, status: 'open', note: 'new' });
    assert.equal(await Ticket.objects.using(db).filter(Ticket.id.eq(4)).count(), 1);
    assert.equal(await OpenTicket.objects.using(db).filter(OpenTicket.id.eq(2)).update({ note: 'changed' }), 0);
    assert.equal(await OpenTicket.objects.using(db).filter(OpenTicket.id.eq(2)).delete(), 0);
    const closed = await Ticket.objects.using(db).filter(Ticket.id.eq(2)).get();
    assert.equal(closed.note, 'kept');
    await db.close();
  `], { cwd: root, encoding: "utf8", env: process.env });
  assert.equal(done.status, 0, done.stderr);
  const warnings = done.stderr.split("\n").filter((line) => line.startsWith('{"code":"acme.outside.shape"')).map((line) => JSON.parse(line) as { model: string; field: string; occurrence_count: number });
  assert.deepEqual(warnings.map((w) => [w.model, w.field, w.occurrence_count]), [["OpenTicket", "note", 1]]);
});
