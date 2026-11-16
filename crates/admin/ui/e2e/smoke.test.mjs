// Browser smoke test of the admin UI. It starts a real `python -m monorm admin` process on SQLite,
// signs in, opens a list and a detail page, and fails on any console error, CSP violation or failed request.
// Needs: the `monorm` wheel in the active Python (`maturin develop`), `npm install` and `npm run install-browser` here.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

model Note {
  id    BigInt @id @default(autoincrement())
  title String
  @@map("notes")
}

model Reviewer {
  id     BigInt  @id @default(autoincrement())
  name   String
  papers Paper[]
  @@map("reviewers")
}

model Paper {
  id        BigInt  @id @default(autoincrement())
  title     String
  reviewer_id BigInt?
  cover     Json?
  reviewer    Reviewer? @relation(fields: [reviewer_id], references: [id])
  @@map("papers")
}

model Ticket {
  id     BigInt @id @default(autoincrement())
  title  String
  status String @default("open")
  @@map("tickets")
}

model Album {
  id     BigInt  @id @default(autoincrement())
  title  String
  tracks Track[]
  @@map("albums")
}

model Track {
  id       BigInt @id @default(autoincrement())
  title    String
  plays    Int    @default(0)
  album_id BigInt
  album    Album  @relation(fields: [album_id], references: [id])
  @@map("tracks")
}
`;

// A custom form: the module default-exports a function from React to a component.
const NOTE_FORM = `export default (React) => function NoteForm({ values, errors, saving, save }) {
  const [title, setTitle] = React.useState(values.title ?? "");
  const h = React.createElement;
  return h("form", { className: "form", onSubmit: (e) => { e.preventDefault(); void save(title === "" ? {} : { title }); } },
    h("label", { htmlFor: "note-title" }, "Note text"),
    h("input", { id: "note-title", value: title, onChange: (e) => setTitle(e.target.value) }),
    ...(errors.fields.title ?? []).map((m) => h("p", { key: m, role: "note" }, m)),
    h("button", { type: "submit", disabled: saving }, "Save note"));
};
`;

// A bulk action on the selected rows, with a confirmation and a message.
const TICKET = `@action(name="close_tickets", label="Close", placement="bulk", confirm="Close the chosen tickets?")
async def close_tickets(ctx, qs):
    await qs.update(status="closed")
    return message(f"Closed {ctx.count} tickets", "success")


admin.register(Ticket, {"actions": [close_tickets]})`;

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
    await models.Reviewer.objects.insert_many([{"name": f"Rev {n:02d}"} for n in range(1, 26)])
    await models.Ticket.objects.insert_many([{"title": f"Ticket {n}"} for n in range(1, 4)])
    (debut,) = await models.Album.objects.insert_many([{"title": "Debut"}]).returning()
    await models.Track.objects.insert_many([{"title": "Intro", "plays": 1, "album_id": debut.id}])
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
  // A search box on Book: the title is a plain column, so the search is "contains". A filter on its author.
  const module = readFileSync(join(dir, "admin.py"), "utf8");
  assert.ok(module.includes("admin.register(Book)"), "the generated admin.py registers Book");
  assert.ok(module.includes("admin.register(Note)"), "the generated admin.py registers Note");
  // Book: a section layout with a collapsed section. Note: a custom form from a module file.
  const book = 'admin.register(Book, {"list": {"search": [Book.title], "filters": [Book.author]}, "form": [{"title": "Basics", "fields": ["title", "created_at"]}, {"title": "More", "fields": ["author_id"], "collapsed": True}]})';
  writeFileSync(join(dir, "note-form.js"), NOTE_FORM);
  // Paper: a picker over Reviewer (with a create modal) and a file field in a memory store.
  assert.ok(module.includes("admin.register(Reviewer)") && module.includes("admin.register(Paper)"), "the generated admin.py registers Reviewer and Paper");
  assert.ok(module.includes("admin.register(Ticket)"), "the generated admin.py registers Ticket");
  assert.ok(module.includes("admin.register(Album)") && module.includes("admin.register(Track)"), "the generated admin.py registers Album and Track");
  const withStorage = module
    .replace("from monorm.admin import Admin\n", "from monorm.admin import Admin, inline\nfrom monorm_file_storage import Registry\nfrom opendal import AsyncOperator\n")
    // Album: its Track rows as an inline that saves with the album.
    .replace("admin.register(Album)", 'admin.register(Album, {"inlines": [inline(Track)]})')
    .replace("admin = Admin()", 'admin = Admin(storage=Registry({"media": AsyncOperator("memory")}))')
    .replace("from monorm.admin import Admin, inline\n", "from monorm.admin import Admin, inline, action, message\n")
    .replace("admin.register(Ticket)", TICKET)
    .replace("admin.register(Reviewer)", 'admin.register(Reviewer, {"list": {"search": [Reviewer.name]}})')
    .replace("admin.register(Paper)", 'admin.register(Paper, {"relations": {"reviewer": {"create": True}}, "files": {"cover": "media"}})');
  writeFileSync(join(dir, "admin.py"), withStorage.replace("admin.register(Book)", book).replace("admin.register(Note)", 'admin.register(Note, {"form_component": "note-form.js"})'));
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
  // A reload aborts a request still in flight, which the failed-request check would report.
  await page.getByRole("heading", { name: /Author/ }).waitFor();

  await page.reload();
  await page.getByRole("heading", { name: /Author/ }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});

test("sort a column and search a list", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).getByRole("link", { name: "Book", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book`);
  const titles = async () => (await page.locator("tbody tr td:nth-child(3)").allTextContents()).map((t) => t.trim());
  await page.getByText("2 records").waitFor();
  // The old rows stay on screen until the new ones arrive, so each step polls for its rows.
  const rowsAre = async (expected) => {
    for (let i = 0; i < 100; i++) {
      if (JSON.stringify(await titles()) === JSON.stringify(expected)) return;
      await page.waitForTimeout(100);
    }
    assert.deepEqual(await titles(), expected);
  };
  await rowsAre(["First", "Second"]);

  // A first click sorts ascending, a second one descending.
  const header = page.getByRole("columnheader", { name: /Title/ });
  await header.getByRole("button").click();
  await page.locator('th[aria-sort="ascending"]').waitFor();
  await rowsAre(["First", "Second"]);
  await header.getByRole("button").click();
  await page.locator('th[aria-sort="descending"]').waitFor();
  await rowsAre(["Second", "First"]);

  await page.getByRole("searchbox", { name: "Search" }).fill("sec");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  // The count has arrived, so closing the context aborts no request that is still in flight.
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});

test("filter a list, keep the view in the URL, and open a tab", async () => {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  const idle = trackRequests(page);
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));

  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).getByRole("link", { name: "Book", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book`);
  const titles = async () => (await page.locator("tbody tr td:nth-child(3)").allTextContents()).map((t) => t.trim());
  const rowsAre = async (expected) => {
    for (let i = 0; i < 100; i++) {
      if (JSON.stringify(await titles()) === JSON.stringify(expected)) return;
      await page.waitForTimeout(100);
    }
    assert.deepEqual(await titles(), expected);
  };
  await page.getByText("2 records").waitFor();
  await rowsAre(["First", "Second"]);

  // A relation filter: pick the author from the list of options, and the chip carries the author's name.
  await page.getByText("Pick a Author").click();
  await page.getByRole("button", { name: "Ada" }).click();
  await page.waitForURL(`${base}/admin/Book?f.author=1`);
  await rowsAre(["First"]);
  await page.getByRole("button", { name: "Remove Ada" }).click();
  await page.waitForURL(`${base}/admin/Book`);
  await rowsAre(["First", "Second"]);

  // One filter: the books with no author. The view is in the URL, and it comes back after a reload.
  await page.getByLabel("Author: empty").selectOption("empty");
  await page.waitForURL(`${base}/admin/Book?f.author%3Aempty=1`);
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  // The filter counts load after the rows, and a reload aborts a request still in flight.
  await idle();
  await page.reload();
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  assert.equal(await page.getByLabel("Author: empty").inputValue(), "empty");
  // Leaving the list cancels a request still in flight, which the failed-request check would report.
  await idle();

  // The detail page links back to the list as the user left it, and the back button does too.
  await page.getByRole("link", { name: "2", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/2`);
  await page.getByRole("heading", { name: /Book/ }).waitFor();
  await page.getByText("Second").first().waitFor();
  await page.getByRole("link", { name: /← Book/ }).click();
  await page.waitForURL(`${base}/admin/Book?f.author%3Aempty=1`);
  await rowsAre(["Second"]);
  await page.getByText("1 record", { exact: true }).waitFor();

  // One tab: the books of the author. A full load aborts a request still in flight, so the list settles first.
  await idle();
  await page.goto(`${base}/admin/Author/1`);
  await page.getByRole("heading", { name: /Author/ }).waitFor();
  await page.getByRole("tab", { name: "Books" }).click();
  await page.waitForURL(`${base}/admin/Author/1?tab=books`);
  await rowsAre(["First"]);
  await page.getByText("1 record", { exact: true }).waitFor();
  assert.deepEqual(problems, []);
  await context.close();
});

// `networkidle` fires once per document load, so a client-side navigation never waits on it. This counts the requests in flight.
function trackRequests(page) {
  const open = new Set();
  page.on("request", (r) => open.add(r));
  page.on("requestfinished", (r) => open.delete(r));
  page.on("requestfailed", (r) => open.delete(r));
  return async () => {
    let quiet = 0;
    while (quiet < 3) {
      quiet = open.size === 0 ? quiet + 1 : 0;
      await page.waitForTimeout(100);
    }
  };
}

async function signedIn() {
  const context = await browser.newContext({ timezoneId: ZONE, locale: "en-US" });
  const page = await context.newPage();
  const problems = [];
  const idle = trackRequests(page);
  page.on("console", (m) => m.type() === "error" && !m.text().includes("status of 401") && problems.push(`console: ${m.text()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()} ${r.failure()?.errorText}`));
  page.on("response", (r) => r.status() >= 400 && r.status() !== 401 && problems.push(`HTTP ${r.status()}: ${r.url()}`));
  await page.goto(`${base}/admin/-/login`);
  await page.getByLabel("Username").fill("ann");
  await page.getByLabel("Password").fill("pw-of-ann");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("navigation", { name: "Resources" }).waitFor();
  await idle();
  return { context, page, problems, idle };
}

test("add a book from the Books tab of an author, then change it, then delete it", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Author/1?tab=books`);
  await page.getByRole("link", { name: "Add", exact: true }).click();
  // The author comes from the tab: the form opens with it filled in.
  await page.waitForURL(`${base}/admin/Book/-/new?author_id=1`);
  await page.getByRole("heading", { name: "Add Book" }).waitFor();
  assert.equal(await page.getByLabel("Author id").inputValue(), "1");
  await page.getByLabel("Title").fill("Third");
  await page.getByLabel("Created at").fill("2026-02-03T10:00:00");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/3`);
  await page.getByText("Third").first().waitFor();

  await page.getByRole("link", { name: "Edit" }).click();
  await page.waitForURL(`${base}/admin/Book/3/edit`);
  await page.getByRole("heading", { name: "Change Book" }).waitFor();
  assert.equal(await page.getByLabel("Title").inputValue(), "Third");
  await page.getByLabel("Title").fill("Third, revised");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Book/3`);
  await page.getByText("Third, revised").first().waitFor();

  await page.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete it" }).click();
  await page.waitForURL(`${base}/admin/Book`);
  await page.getByText("2 records").waitFor();
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("a form shows the field error of the server, and the edit of an author is saved", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Author/-/new`);
  await page.getByRole("heading", { name: "Add Author" }).waitFor();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByText("This field is required.").waitFor();
  await page.getByLabel("Name").fill("Zed");
  await page.getByRole("button", { name: "Save and continue" }).click();
  await page.waitForURL(`${base}/admin/Author/2/edit`);
  await page.getByLabel("Name").fill("Zed Two");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Author/2`);
  await page.getByText("Zed Two").first().waitFor();
  await page.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete it" }).click();
  await page.waitForURL(`${base}/admin/Author`);
  await page.getByText("1 record", { exact: true }).waitFor();
  await idle();
  // The 422 of the first save is the one error that the test expects: the response and the console line of the browser.
  assert.deepEqual(problems, [`HTTP 422: ${base}/admin/api/resources/Author`, "console: Failed to load resource: the server responded with a status of 422 (Unprocessable Entity)"]);
  await context.close();
});

test("a section layout with a collapsed section, and a custom form", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Book/-/new`);
  await page.getByRole("heading", { name: "Add Book" }).waitFor();
  await page.getByText("Basics", { exact: true }).waitFor();
  const more = page.locator("details.section");
  assert.equal(await more.getAttribute("open"), null, "the collapsed section starts closed");
  assert.equal(await page.getByLabel("Author id").isVisible(), false);
  await more.locator("summary").click();
  await page.getByLabel("Author id").waitFor();
  assert.equal(await page.locator("fieldset.section").count(), 1);

  await idle();
  // The module of the resource replaces the form. The first save has no title, and the 422 comes back as `errors`.
  await page.goto(`${base}/admin/Note/-/new`);
  await page.getByLabel("Note text").waitFor();
  await page.getByRole("button", { name: "Save note" }).click();
  await page.getByRole("note").getByText("This field is required.").waitFor();
  await page.getByLabel("Note text").fill("Hello");
  await page.getByRole("button", { name: "Save note" }).click();
  await page.waitForURL(`${base}/admin/Note/1`);
  await page.getByText("Hello").first().waitFor();
  await idle();
  assert.deepEqual(problems, [`HTTP 422: ${base}/admin/api/resources/Note`, "console: Failed to load resource: the server responded with a status of 422 (Unprocessable Entity)"]);
  await context.close();
});

test("pick a record with the autocomplete, create one in a modal, and upload a file", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Paper/-/new`);
  await page.getByRole("heading", { name: "Add Paper" }).waitFor();
  await page.getByLabel("Title").fill("Pick");
  const reviewer = page.getByRole("combobox", { name: "Reviewer id" });
  await reviewer.click();
  const options = page.getByRole("listbox").getByRole("option");
  await options.nth(19).waitFor();
  assert.equal(await options.count(), 20, "the first page holds 20 rows");
  await page.getByRole("button", { name: "Show more" }).click();
  await options.nth(24).waitFor();
  assert.equal(await options.count(), 25, "the next page follows by keyset");
  // A search narrows the list on the server. Each step waits for its rows.
  await reviewer.fill("Rev 23");
  await page.getByRole("option", { name: "Rev 23" }).waitFor();
  await options.nth(1).waitFor({ state: "detached" });
  await page.getByRole("option", { name: "Rev 23" }).click();
  assert.equal(await reviewer.inputValue(), "Rev 23");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Paper/1`);
  await page.getByText("23").first().waitFor();

  // The modal creates a Reviewer, fills the field and leaves the other fields of the form as they were.
  await idle();
  await page.goto(`${base}/admin/Paper/-/new`);
  await page.getByLabel("Title").fill("Modal");
  await page.getByRole("combobox", { name: "Reviewer id" }).click();
  await page.getByRole("button", { name: /^New Reviewer/ }).click();
  const dialog = page.getByRole("dialog", { name: "New Reviewer" });
  await dialog.getByLabel("Name").fill("Rev New");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(await page.getByRole("combobox", { name: "Reviewer id" }).inputValue(), "Rev New");
  assert.equal(await page.getByLabel("Title").inputValue(), "Modal", "the form keeps its values");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Paper/2`);
  await page.getByText("26").first().waitFor();

  // One upload through the multipart route, then the download as an attachment.
  await idle();
  await page.goto(`${base}/admin/Paper/2/edit`);
  await page.getByRole("heading", { name: "Change Paper" }).waitFor();
  await page.getByLabel("Cover").setInputFiles({ name: "cover.txt", mimeType: "text/plain", buffer: Buffer.from("cover bytes") });
  const link = page.getByRole("link", { name: "cover.txt" });
  await link.waitFor();
  const got = await context.request.get(new URL(await link.getAttribute("href"), base).href);
  assert.equal(got.status(), 200);
  assert.match(got.headers()["content-disposition"], /^attachment/);
  assert.equal(got.headers()["content-type"], "text/plain");
  assert.equal(await got.text(), "cover bytes");
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("change an inline row and add another with the album, in one save", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Album/1/edit`);
  await page.getByRole("heading", { name: "Change Album" }).waitFor();
  const tracks = page.locator("details.inline");
  await tracks.getByText("Tracks", { exact: true }).waitFor();
  const titles = tracks.getByLabel("Title");
  assert.equal(await titles.first().inputValue(), "Intro");
  await titles.first().fill("Intro, revised");
  await tracks.getByRole("button", { name: "Add row" }).click();
  await titles.nth(1).fill("Outro");
  await tracks.getByLabel("Plays").nth(1).fill("7");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.waitForURL(`${base}/admin/Album/1`);
  // The detail page shows the stored rows.
  await page.getByRole("cell", { name: "Intro, revised" }).waitFor();
  await page.getByRole("cell", { name: "Outro" }).waitFor();
  const stored = await (await context.request.get(`${base}/admin/api/resources/Album/1`)).json();
  const rows = stored.inlines.tracks.rows.map((r) => JSON.stringify(r.values));
  assert.equal(rows.length, 2);
  assert.ok(rows[0].includes("Intro, revised") && rows[1].includes("Outro") && rows[1].includes("7"), rows.join(" | "));
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});

test("run a bulk action on the selected rows, and on all rows that match", async () => {
  const { context, page, problems, idle } = await signedIn();
  await page.goto(`${base}/admin/Ticket?limit=2`);
  await page.getByRole("cell", { name: "Ticket 1" }).waitFor();
  assert.equal(await page.getByRole("group", { name: "Bulk actions" }).count(), 0, "no bar before a selection");
  await page.getByLabel("Select row 1").check();
  await page.getByLabel("Select row 2").check();
  await page.getByText("2 selected").waitFor();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  const dialog = page.getByRole("alertdialog", { name: "Confirm Close" });
  await dialog.getByText("Close the chosen tickets?").waitFor();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  // The message shows, the selection ends and the rows reload.
  await page.getByRole("status").filter({ hasText: "Closed 2 tickets" }).waitFor();
  await page.getByRole("group", { name: "Bulk actions" }).waitFor({ state: "detached" });
  const rows = (await (await context.request.get(`${base}/admin/api/resources/Ticket`)).json()).rows.map((r) => JSON.stringify(r));
  assert.equal(rows.filter((r) => r.includes("closed")).length, 2, rows.join(" | "));
  // The whole page, then all rows that match: the count comes from the server.
  await page.getByLabel("Select all rows of this page").check();
  await page.getByRole("button", { name: /^Select all 3 matching/ }).click();
  await page.getByText("All 3 matching records selected").waitFor();
  await page.getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("alertdialog", { name: "Confirm Close" }).getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("status").filter({ hasText: "Closed 3 tickets" }).waitFor();
  await idle();
  assert.deepEqual(problems, []);
  await context.close();
});
