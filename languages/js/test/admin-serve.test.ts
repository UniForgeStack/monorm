/** `npx monorm admin`: the server of a project on SQLite, started as a process of its own. */

import assert from "node:assert/strict";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { PACKAGE } from "./auth/support.js";

const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

const SCHEMA = 'datasource db {\n  provider = "sqlite"\n}\n\nmodel Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n\n  @@map("books")\n}\n';

function cli(args: string[], cwd: string, url = ""): { code: number; out: string; err: string } {
  const r = spawnSync(process.execPath, [CLI, ...args], { cwd, encoding: "utf8", env: { ...process.env, MONORM_DATABASE_URL: url } });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
}

const AUTH_SCHEMA = 'datasource db {\n  provider = "sqlite"\n}\nimport "app:auth" (prefix: "auth_")\nimport "app:admin" (prefix: "admin_")\n';

function project(migrate = true, schema = SCHEMA): { dir: string; url: string } {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-serve-"));
  writeFileSync(join(dir, "schema.prisma"), schema);
  writeFileSync(join(dir, "package.json"), '{"type":"module"}');
  mkdirSync(join(dir, "node_modules"));
  symlinkSync(PACKAGE, join(dir, "node_modules/monorm"));
  assert.equal(cli(["admin", "init"], dir).code, 0);
  assert.equal(cli(["makemigrations"], dir).code, 0);
  const url = `sqlite://${dir}/admin.db`;
  if (migrate) assert.equal(cli(["migrate", "--url", url], dir).code, 0);
  return { dir, url };
}

interface Running {
  readonly child: ChildProcess;
  readonly stderr: () => string;
  readonly ended: Promise<number>;
  listening(): Promise<string>;
}

const SECRET = "s".repeat(40);

function start(dir: string, url: string, env: Record<string, string | undefined> = {}): Running {
  const child = spawn(process.execPath, [CLI, "admin"], { cwd: dir, env: { ...process.env, MONORM_ADMIN_PORT: "0", MONORM_DATABASE_URL: url, MONORM_ADMIN_SECRET: SECRET, ...env } });
  let err = "";
  child.stderr!.setEncoding("utf8");
  child.stderr!.on("data", (chunk: string) => (err += chunk));
  const ended = new Promise<number>((done) => child.on("close", (code) => done(code ?? -1)));
  const listening = async (): Promise<string> => {
    for (let i = 0; i < 500; i++) {
      const line = err.split("\n").find((l) => l.includes('"listening"'));
      if (line) return (JSON.parse(line) as { url: string }).url;
      await new Promise((r) => setTimeout(r, 20));
    }
    throw new Error(`the server did not listen: ${err}`);
  };
  return { child, stderr: () => err, ended, listening };
}

async function get(url: string): Promise<{ status: number; id: string | null }> {
  const r = await fetch(url);
  return { status: r.status, id: r.headers.get("x-request-id") };
}

test("monorm admin serves health with a request id and stops on sigterm", async () => {
  const { dir, url } = project();
  const run = start(dir, url);
  const base = await run.listening();
  assert.equal((await get(`${base}/healthz`)).status, 200);
  assert.equal((await get(`${base}/readyz`)).status, 200);
  const api = await get(`${base}/admin/api/resources`);
  assert.equal(api.status, 401);
  assert.ok(api.id && api.id.length > 0);
  run.child.kill("SIGTERM");
  assert.equal(await run.ended, 0, run.stderr());
  assert.ok(run.stderr().includes('"request_id"'));
});

test("monorm admin fails with a clear error when the admin tables are absent", async () => {
  const { dir, url } = project(false);
  const run = start(dir, url);
  assert.equal(await run.ended, 1);
  assert.match(run.stderr(), /^error: the admin table .*run makemigrations and migrate/s);
});

test("monorm admin exits 2 without a long enough secret", async () => {
  const { dir, url } = project();
  for (const secret of ["", "short", undefined]) {
    const run = start(dir, url, { MONORM_ADMIN_SECRET: secret });
    assert.equal(await run.ended, 2, run.stderr());
    assert.match(run.stderr(), /MONORM_ADMIN_SECRET/);
  }
});

test("monorm admin names the wrong setting and the missing database", async () => {
  const { dir, url } = project();
  let run = start(dir, "");
  assert.equal(await run.ended, 1);
  assert.match(run.stderr(), /MONORM_DATABASE_URL/);
  writeFileSync(join(dir, "monorm.toml"), '[admin]\napp = "./admin.ts:admin"\nthreads = 4\n');
  run = start(dir, url);
  assert.equal(await run.ended, 2);
  assert.match(run.stderr(), /`admin.threads` is not a setting/);
  writeFileSync(join(dir, "monorm.toml"), '[admin]\napp = "./admin.ts:nothing"\n');
  run = start(dir, url);
  assert.equal(await run.ended, 1);
  assert.match(run.stderr(), /admin\.app `\.\/admin\.ts:nothing`/);
});

test("the setup hook connects the database instead of the environment", async () => {
  const { dir, url } = project();
  writeFileSync(join(dir, "hooks.ts"), `import { connect } from "monorm";\n\nexport async function setup() {\n  await connect(${JSON.stringify(url)});\n}\n`);
  writeFileSync(join(dir, "monorm.toml"), '[admin]\napp = "./admin.ts:admin"\nsetup = "./hooks.ts:setup"\n');
  const run = start(dir, "");
  assert.equal((await get(`${await run.listening()}/readyz`)).status, 200);
  run.child.kill("SIGTERM");
  assert.equal(await run.ended, 0, run.stderr());
});

test("two processes run side by side on one database", async () => {
  const { dir, url } = project();
  const first = start(dir, url);
  const second = start(dir, url);
  const [a, b] = [await first.listening(), await second.listening()];
  assert.notEqual(a, b);
  assert.equal((await get(`${a}/readyz`)).status, 200);
  assert.equal((await get(`${b}/readyz`)).status, 200);
  first.child.kill("SIGTERM");
  second.child.kill("SIGTERM");
  assert.equal(await first.ended, 0);
  assert.equal(await second.ended, 0);
});

test("monorm admin sync permissions creates the names and a second run creates none", () => {
  const { dir, url } = project(true, AUTH_SCHEMA);
  const first = cli(["admin", "sync-permissions"], dir, url);
  assert.equal(first.code, 0, first.err);
  assert.match(first.out, /permissions: 19 created, 0 stale/);
  const second = cli(["admin", "sync-permissions"], dir, url);
  assert.equal(second.code, 0, second.err);
  assert.match(second.out, /permissions: 0 created, 0 stale/);
});

test("monorm admin sync permissions names a schema without auth models", () => {
  const { dir, url } = project();
  const r = cli(["admin", "sync-permissions"], dir, url);
  assert.equal(r.code, 2, r.err);
  assert.match(r.err, /no auth model Permission/);
});

test("monorm admin sync permissions seeds the roles of the admin and a second run adds none", () => {
  const { dir, url } = project(true, AUTH_SCHEMA);
  const file = join(dir, "admin.ts");
  writeFileSync(file, readFileSync(file, "utf8").replace("new Admin()", 'new Admin(undefined, { roles: { reader: ["admin.access", "admin.read_*"] } })'));
  const first = cli(["admin", "sync-permissions"], dir, url);
  assert.equal(first.code, 0, first.err);
  assert.match(first.out, /roles: 1 created, 10 grants added/);
  const second = cli(["admin", "sync-permissions"], dir, url);
  assert.equal(second.code, 0, second.err);
  assert.match(second.out, /roles: 0 created, 0 grants added/);
});
