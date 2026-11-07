// Browser smoke test of the admin UI. It starts a real `python -m monorm admin` process on SQLite,
// signs in, opens a list and a detail page, and fails on any console error, CSP violation or failed request.
// Needs: the `monorm` wheel in the active Python (`maturin develop`), `npm install` and `npm run install-browser` here.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
const PYTHON = process.env.MONORM_PYTHON ?? "python";
const SECRET = "smoke-secret-of-at-least-32-bytes!";
// 2026-01-02T03:04:05Z in this zone is 6:34:05 AM (UTC+03:30, no daylight saving time since 2022).
const ZONE = "Asia/Tehran";

const SCHEMA = `datasource db {
  provider = "sqlite"
}
import "app:monorm-auth" (prefix: "auth_")

model Author {
  id    BigInt @id @default(autoincrement())
  name  String
  books Book[]
  @@map("authors")
}

model Book {
  id         BigInt   @id @default(autoincrement())
  title      String
  created_at DateTime
  author_id  BigInt?
  author     Author?  @relation(fields: [author_id], references: [id])
  @@map("books")
}
`;

const SEED = `
import asyncio, datetime, os
import monorm
from monorm_auth import make_password
import models

async def main():
    db = await monorm.connect(os.environ["MONORM_DATABASE_URL"])
    await models.User.objects.insert_many([{"username": "ann", "email": "ann@example.com", "password": make_password("pw-of-ann"), "is_superuser": True}])
    (ada,) = await models.Author.objects.insert_many([{"name": "Ada"}]).returning()
    at = datetime.datetime(2026, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    await models.Book.objects.insert_many([
        {"title": "First", "created_at": at, "author_id": ada.id},
        {"title": "Second", "created_at": at, "author_id": None},
    ])
    await db.close()

asyncio.run(main())
`;

let dir, server, base, browser;
const stderr = [];

function run(args, env) {
  const done = spawnSync(PYTHON, args, { cwd: dir, env, encoding: "utf8" });
  assert.equal(done.status, 0, `${PYTHON} ${args.join(" ")}\n${done.stdout}${done.stderr}`);
}

before(async () => {
  dir = mkdtempSync(join(tmpdir(), "monorm-admin-smoke-"));
  // The built-in apps import from the source tree when `monorm` is an editable install; the root makes that path resolve.
  const env = { ...process.env, PYTHONPATH: ROOT, MONORM_DATABASE_URL: `sqlite://${dir}/app.db`, MONORM_ADMIN_SECRET: SECRET, MONORM_ADMIN_PORT: "0" };
  writeFileSync(join(dir, "schema.prisma"), SCHEMA);
  writeFileSync(join(dir, "seed.py"), SEED);
  run(["-m", "monorm", "admin", "init", "--models", "models"], env);
  run(["-m", "monorm", "generate"], env);
  run(["-m", "monorm", "makemigrations"], env);
  run(["-m", "monorm", "migrate", "--url", env.MONORM_DATABASE_URL], env);
  run(["seed.py"], env);
  server = spawn(PYTHON, ["-m", "monorm", "admin"], { cwd: dir, env, stdio: ["ignore", "ignore", "pipe"] });
  base = await new Promise((resolveUrl, reject) => {
    let text = "";
    server.stderr.on("data", (chunk) => {
      text += chunk;
      stderr.push(String(chunk));
      const found = /"url":"([^"]+)"/.exec(text);
      if (found && text.includes('"listening"')) resolveUrl(found[1]);
    });
    server.on("exit", (code) => reject(new Error(`the admin ended with ${code}\n${text}`)));
  });
  browser = await chromium.launch();
  // A server that never listens ends the hook, not the CI job.
}, { timeout: 120_000 });

after(async () => {
  await browser?.close();
  if (server && server.exitCode === null) {
    server.removeAllListeners("exit");
    server.kill("SIGTERM");
  }
  if (dir) rmSync(dir, { recursive: true, force: true });
});

test("sign in, open a list and a detail page", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

  await page.goto(`${base}/admin/`);
  await page.waitForURL(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("wrong");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("alert").waitFor();
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();

  const nav = page.getByRole("navigation", { name: "Resources" });
  await nav.getByRole("link", { name: "Book", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book`);
  await page.getByRole("link", { name: "1", exact: true }).waitFor();
  assert.ok(await page.getByText("6:34:05 AM").first().isVisible(), "the time shows in the browser time zone");

  await page.getByRole("link", { name: "1", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/1`);
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  await page.getByText("First").first().waitFor();
  await page.getByRole("link", { name: "Author 1" }).click();
  await page.waitForURL(`${base}/admin/Author/1`);

  await page.reload();
  await page.getByRole("heading", { name: /Author/ }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});
