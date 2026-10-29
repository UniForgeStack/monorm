/** `import "app:<name>"`: the npm resolution through `"monorm": {"app": ...}` in `package.json`. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test, type TestContext } from "node:test";
import { Registry, SchemaError, load, loads } from "../src/index.js";
import { resolveApps } from "../src/apps.js";
import { native } from "../src/native.js";
import { resolveExtensions } from "../src/packages.js";

const PROJECT = `datasource db { provider = "sqlite" }
import "app:acme-auth" (prefix: "auth_")
model Post { id Int @id }
`;
const AUTH = `import "app:monorm-billing" (prefix: "b_")
model User { id Int @id }
`;
const BILLING = "model Invoice { id Int @id }\n";

function write(file: string, text: string): string {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, text);
  return file;
}

function directory(t: TestContext): string {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), "monorm-app-imports-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** An npm package `name` under `root`, with its schema, and `"monorm": {"app": ...}` unless `app` is null. */
function pkg(root: string, name: string, schema: string, app: string | null = "./schema.prisma"): string {
  write(join(root, name, "package.json"), JSON.stringify({ name, version: "1.0.0", ...(app ? { monorm: { app } } : {}) }));
  return write(join(root, name, "schema.prisma"), schema);
}

const tables = (ir: string): string[] => (JSON.parse(ir) as { models: { table: string }[] }).models.map((m) => m.table);

test("an npm package and the app it imports resolve from node_modules", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), PROJECT);
  const auth = pkg(join(dir, "node_modules"), "acme-auth", AUTH);
  const billing = pkg(join(dir, "node_modules"), "monorm-billing", BILLING);
  const apps = resolveApps(schema);
  assert.deepEqual(apps, { "acme-auth": auth, "monorm-billing": billing });
  assert.deepEqual(tables(native().compileSchemaFile(schema, apps)), ["auth_b_invoice", "auth_user", "post"]);
});

test("an app that only another app depends on resolves in pnpm's strict layout", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), PROJECT);
  const store = join(dir, "node_modules", ".pnpm");
  const auth = pkg(join(store, "acme-auth@1.0.0", "node_modules"), "acme-auth", AUTH);
  const billing = pkg(join(store, "monorm-billing@1.0.0", "node_modules"), "monorm-billing", BILLING);
  // the project sees only acme-auth; acme-auth sees monorm-billing as a sibling in its own store entry
  symlinkSync(dirname(auth), join(dir, "node_modules", "acme-auth"));
  symlinkSync(dirname(billing), join(store, "acme-auth@1.0.0", "node_modules", "monorm-billing"));
  const apps = resolveApps(schema);
  assert.deepEqual(apps, { "acme-auth": auth, "monorm-billing": billing });
  assert.deepEqual(tables(native().compileSchemaFile(schema, apps)), ["auth_b_invoice", "auth_user", "post"]);
});

test("an app that is not installed is left for the loader to report", (t) => {
  const schema = write(join(directory(t), "schema.prisma"), PROJECT);
  assert.deepEqual(resolveApps(schema), {});
  assert.throws(() => native().compileSchemaFile(schema, {}), /app:acme-auth.*not installed.*install the `acme-auth` package/s);
});

test("an app without schema metadata names the missing package.json key", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), PROJECT);
  pkg(join(dir, "node_modules"), "acme-auth", AUTH, null);
  assert.throws(() => resolveApps(schema), (e) => e instanceof SchemaError && /`acme-auth` package declares no schema.*"monorm"/s.test(e.message));
});

test("an app whose package.json names no file is an error", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), PROJECT);
  pkg(join(dir, "node_modules"), "acme-auth", AUTH, "./missing.prisma");
  assert.throws(() => resolveApps(schema), (e) => e instanceof SchemaError && /missing.prisma.*not a file/s.test(e.message));
});

// `npx monorm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

test("the command line resolves apps before it runs a command", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), PROJECT);
  const run = () => {
    const r = spawnSync(process.execPath, [CLI, "--schema", schema, "check"], { encoding: "utf8", env: { ...process.env, MONORM_DATABASE_URL: "" } });
    return { code: r.status, out: r.stdout, err: r.stderr };
  };
  assert.match(run().err, /not installed/);
  pkg(join(dir, "node_modules"), "acme-auth", AUTH);
  pkg(join(dir, "node_modules"), "monorm-billing", BILLING, null);
  const broken = run();
  assert.equal(broken.code, 1);
  assert.match(broken.err, /`monorm-billing` package declares no schema/);
  pkg(join(dir, "node_modules"), "monorm-billing", BILLING);
  const ok = run();
  assert.equal(ok.code, 0, ok.err);
  assert.match(ok.out, /\(3 models\)/);
});

test("load resolves an installed app and loads names the way out", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), PROJECT);
  pkg(join(dir, "node_modules"), "acme-auth", AUTH);
  pkg(join(dir, "node_modules"), "monorm-billing", BILLING);
  assert.deepEqual(Object.keys(load(schema, { registry: new Registry() })).sort(), ["Invoice", "Post", "User"]);
  assert.throws(() => loads(PROJECT, { registry: new Registry() }), /app:acme-auth.*use `load\(path\)` or generate/s);
});

const BUILTIN = `datasource db { provider = "sqlite" }
import "app:auth" (prefix: "auth_")
model Post { id Int @id }
`;

test("the auth app comes with monorm and needs no install", (t) => {
  const dir = directory(t);
  write(join(dir, "package.json"), "{}");
  const schema = write(join(dir, "schema.prisma"), BUILTIN);
  const apps = resolveApps(schema);
  assert.deepEqual(Object.keys(apps), ["auth"]);
  assert.ok(apps["auth"]?.endsWith(join("auth", "schema.prisma")));
  assert.ok(tables(native().compileSchemaFile(schema, apps, resolveExtensions(schema))).includes("auth_user"));
});

test("the bundled auth app wins over an installed package of the same name", (t) => {
  const dir = directory(t);
  const schema = write(join(dir, "schema.prisma"), BUILTIN);
  pkg(join(dir, "node_modules"), "auth", "model Other { id Int @id }\n");
  assert.ok(resolveApps(schema)["auth"]?.endsWith(join("auth", "schema.prisma")));
  assert.ok(!resolveApps(schema)["auth"]?.includes("node_modules"));
});
