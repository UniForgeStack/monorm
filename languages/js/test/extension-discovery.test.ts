/** Extension packages: the npm discovery through `"orm": {"extension": ...}` in the direct dependencies. */
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test, type TestContext } from "node:test";
import { load, loads, Registry, SchemaError } from "../src/index.js";
import { resolveExtensions } from "../src/packages.js";

// `npx orm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;
const MANIFEST = 'id = "acme.fixture"\nversion = "0.1.0"\nabi = "0.1"\nlibrary = "libfixture"\n';

function write(file: string, text: string): string {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, text);
  return file;
}

function directory(t: TestContext): string {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "orm-extensions-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** An installed package `name` under `root/node_modules`, with the extension id and manifest unless null. */
function pkg(root: string, name: string, id: string | null, manifest: boolean = true): string {
  const dir = join(root, "node_modules", name);
  write(join(dir, "package.json"), JSON.stringify({ name, version: "1.0.0", ...(id ? { orm: { extension: id } } : {}) }));
  return manifest ? write(join(dir, "orm-extension.toml"), MANIFEST) : dir;
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
  assert.throws(() => resolveExtensions(join(root, "schema.prisma")), (e: unknown) => e instanceof SchemaError && /acme-fixture.*orm-extension\.toml/.test(e.message));
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
  const target = realpathSync(mkdtempSync(join(tmpdir(), "orm-outside-")));
  t.after(() => rmSync(target, { recursive: true, force: true }));
  execFileSync("cargo", ["build", "--offline", "--quiet", "--manifest-path", join(OUTSIDE, "Cargo.toml"), "--target-dir", target], { stdio: "inherit" });
  const file = readdirSync(join(target, "debug")).find((f) => /^liboutside_extension\.(dylib|so|dll)$/.test(f));
  assert.ok(file, "the outside library is built");
  return join(target, "debug", file);
}

test("an installed outside library lowers the schema through load(), loads() and the command line", { timeout: 300_000 }, (t) => {
  const root = directory(t);
  const manifest = pkg(root, "acme-outside", "acme.outside", false) + "/orm-extension.toml";
  write(manifest, readFileSync(join(OUTSIDE, "orm-extension.toml"), "utf8"));
  const library = outsideLibrary(t);
  copyFileSync(library, join(dirname(manifest), library.slice(library.lastIndexOf("/") + 1)));
  project(root, { "acme-outside": "1.0.0" });
  const schema = write(join(root, "schema.prisma"), 'model Post {\n id Int @id\n @@outside.tag("blue")\n}\n');
  const { Post } = load(schema, { registry: new Registry() });
  assert.ok(Post?._meta.fields.has("tag"), "the pass adds the field");

  // `loads()` has no file, so the extensions come from the working directory.
  const cwd = process.cwd();
  process.chdir(root);
  t.after(() => process.chdir(cwd));
  const loaded = loads('model Post {\n id Int @id\n @@outside.tag("blue")\n}\n', { registry: new Registry() });
  assert.ok(loaded.Post?._meta.fields.has("tag"), "loads() finds the library from the working directory");

  // The command line resolves the extensions from the schema file, in a process of its own.
  const run = (...args: string[]) => {
    const done = spawnSync(process.execPath, [CLI, ...args], { cwd: root, encoding: "utf8", env: { ...process.env, ORM_DATABASE_URL: "" } });
    assert.equal(done.status, 0, done.stderr);
    return done.stdout;
  };
  run("check");
  run("makemigrations");
  assert.ok(run("sqlmigrate", "1").includes('"tag"'), "the migration adds the column of the pass");
  run("generate");
  run("check");
  const original = readFileSync(manifest, "utf8");
  writeFileSync(manifest, original + "\n# changed after generation\n");
  const stale = spawnSync(process.execPath, [CLI, "check"], { cwd: root, encoding: "utf8" });
  assert.equal(stale.status, 1, stale.stderr);
  assert.match(stale.stderr, /acme.outside/);
  assert.match(stale.stderr, /run `orm generate`/);
  writeFileSync(manifest, original);
  run("check");
});
