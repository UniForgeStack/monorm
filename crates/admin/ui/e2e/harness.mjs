// The project that the browser tests and `shots.mjs` drive: a `monorm admin` process on SQLite with seeded data and the example package.
// Needs the `monorm` wheel in the active Python (`maturin develop`).
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
export const PYTHON = process.env.MONORM_PYTHON ?? "python";
export const SECRET = "smoke-secret-of-at-least-32-bytes!";
// 2026-01-02T03:04:05Z in this zone is 6:34:05 AM (UTC+03:30, no daylight saving time since 2022).
export const ZONE = "Asia/Tehran";

export const SCHEMA = `datasource db {
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
  tracks Track[]
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

model Movie {
  id    BigInt @id @default(autoincrement())
  title String
  stars Int?
  @@map("movies")
}

model Shelf {
  id    BigInt @id @default(autoincrement())
  title String @db.VarChar(8)
  qty   Int    @default(0)
  @@map("shelves")
}

model Album {
  id     BigInt  @id @default(autoincrement())
  title  String
  tracks Track[]
  @@map("albums")
}

// A protobuf message in a Bytes field: the admin shows and edits it as JSON.
model Setting {
  id     BigInt @id @default(autoincrement())
  name   String
  config Bytes? @proto.message("acme.Note")
  @@map("settings")
}

// Every kind of picker and control: a date, a time, a date with a time, a switch, an enum and JSON.
enum Mood {
  calm
  tense
  @@storage(text)
}

model Event {
  id      BigInt   @id @default(autoincrement())
  title   String
  day     DateTime @db.Date
  at      DateTime @db.Time
  starts  DateTime
  live    Boolean  @default(false)
  mood    Mood     @default(calm)
  extra   Json?
  @@map("events")
}

// Display helpers and the date drill-down: a bool icon, badges on a text column, and a date hierarchy on a datetime.
model Parcel {
  id     BigInt   @id @default(autoincrement())
  code   String
  placed DateTime
  paid   Boolean  @default(false)
  state  String   @default("new")
  @@map("parcels")
}

model Track {
  id       BigInt @id @default(autoincrement())
  title    String
  plays    Int    @default(0)
  album_id BigInt
  album    Album  @relation(fields: [album_id], references: [id])
  // A picker in an inline row: its list opens inside the table.
  reviewer_id BigInt?
  reviewer    Reviewer? @relation(fields: [reviewer_id], references: [id])
  @@map("tracks")
}

// A radio group: the widget map of its admin sets it.
enum Tier {
  free
  pro
  @@storage(text)
}

model Gadget {
  id   BigInt @id @default(autoincrement())
  name String
  tier Tier
  @@map("gadgets")
}
`;

// A custom form: the module default-exports a function from React to a component.
export const NOTE_FORM = `export default (React) => function NoteForm({ values, errors, saving, save }) {
  const [title, setTitle] = React.useState(values.title ?? "");
  const h = React.createElement;
  return h("form", { className: "form", onSubmit: (e) => { e.preventDefault(); void save(title === "" ? {} : { title }); } },
    h("label", { htmlFor: "note-title" }, "Note text"),
    h("input", { id: "note-title", value: title, onChange: (e) => setTitle(e.target.value) }),
    ...(errors.fields.title ?? []).map((m) => h("p", { key: m, role: "note" }, m)),
    h("button", { type: "submit", disabled: saving }, "Save note"));
};
`;

// A bulk action on the selected rows, with a confirmation and a message, and a row action in the row menu.
export const TICKET = `@action(name="close_tickets", label="Close", placement="bulk", confirm="Close the chosen tickets?", can=Ticket.id > 0, replace_change=True)
async def close_tickets(ctx, qs):
    await qs.update(status="closed")
    return message(f"Closed {ctx.count} tickets", "success")


@action(name="reopen_ticket", label="Reopen", placement="row", can=Ticket.status == "closed", replace_change=True)
async def reopen_ticket(ctx, qs):
    await qs.update(status="open")
    return message("Reopened", "success")


@action(label="Review", placement="detail", can=Ticket.status == "open", replace_change=True)
async def review_ticket(ctx, qs):
    return message("Reviewed", "success")


@action(label="Mark closed", placement="submit", can=Ticket.status == "closed")
async def mark_closed(ctx, qs):
    return message("Marked closed", "success")


admin.register(Ticket, {
    "can": {"change": Ticket.status != "closed"},
    "actions": [close_tickets, reopen_ticket, review_ticket, mark_closed],
})`;

export const HELD_ROLES = `
import asyncio, os
import monorm
import models

async def main():
    db = await monorm.connect(os.environ["MONORM_DATABASE_URL"])
    user = await models.User.objects.using(db).filter(models.User.username == "sso@example.com").first()
    held = await models.UserRole.objects.using(db).filter(models.UserRole.user_id == user.id)
    roles = await models.Role.objects.using(db).filter(models.Role.id.in_([j.role_id for j in held]))
    print(",".join(sorted(r.name for r in roles)))

asyncio.run(main())
`;

export const SEED = `
import asyncio, datetime, os
import monorm
from monorm_auth import make_password
import models
from acme import note_pb2

async def main():
    db = await monorm.connect(os.environ["MONORM_DATABASE_URL"])
    await models.User.objects.insert_many([{"username": "ann", "email": "ann@example.com", "password": make_password("pw-of-ann"), "is_superuser": True}])
    await models.Role.objects.insert_many([{"name": "support"}])
    (ada,) = await models.Author.objects.insert_many([{"name": "Ada"}]).returning()
    at = datetime.datetime(2026, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    await models.Book.objects.insert_many([
        {"title": "First", "created_at": at, "author_id": ada.id},
        {"title": "Second", "created_at": at, "author_id": None},
    ])
    await models.Reviewer.objects.insert_many([{"name": f"Rev {n:02d}"} for n in range(1, 26)])
    await models.Ticket.objects.insert_many([{"title": f"Ticket {n}"} for n in range(1, 4)])
    await models.Movie.objects.insert_many([{"title": "Alpha", "stars": 4}, {"title": "Beta", "stars": 2}])
    await models.Shelf.objects.insert_many([{"title": "Oak", "qty": 1}, {"title": "Pine", "qty": 2}, {"title": "Elm", "qty": 3}])
    await models.Event.objects.insert_many([{"title": "Launch", "day": datetime.date(2026, 3, 4), "at": datetime.time(9, 30, 15), "starts": at, "live": True, "mood": "tense", "extra": {"a": 1}}])
    await models.Event.objects.insert_many([{"title": "Whole minutes", "day": datetime.date(2026, 5, 6), "at": datetime.time(10, 0, 0), "starts": datetime.datetime(2026, 1, 2, 4, 0, 0, tzinfo=datetime.timezone.utc), "mood": "calm"}])
    await models.Gadget.objects.insert_many([{"name": "Widget", "tier": "free"}])
    utc = datetime.timezone.utc
    await models.Parcel.objects.insert_many([
        {"code": "P-2024-03", "placed": datetime.datetime(2024, 3, 5, 12, 0, 0, tzinfo=utc), "paid": True, "state": "shipped"},
        {"code": "P-2025-03-a", "placed": datetime.datetime(2025, 3, 10, 12, 0, 0, tzinfo=utc), "paid": True, "state": "new"},
        {"code": "P-2025-03-b", "placed": datetime.datetime(2025, 3, 20, 12, 0, 0, tzinfo=utc), "paid": False, "state": "lost"},
        {"code": "P-2025-04", "placed": datetime.datetime(2025, 4, 2, 12, 0, 0, tzinfo=utc), "paid": False, "state": "returned"},
        {"code": "P-2026-01", "placed": datetime.datetime(2026, 1, 8, 12, 0, 0, tzinfo=utc), "paid": True, "state": "shipped"},
    ])
    await models.Setting.objects.insert_many([{"name": "Welcome", "config": note_pb2.Note(title="hi", priority=3, tags=["a"])}])
    (debut,) = await models.Album.objects.insert_many([{"title": "Debut"}]).returning()
    await models.Track.objects.insert_many([{"title": "Intro", "plays": 1, "album_id": debut.id}])
    await db.close()

asyncio.run(main())
`;

function run(dir, args, env) {
  const done = spawnSync(PYTHON, args, { cwd: dir, env, encoding: "utf8" });
  assert.equal(done.status, 0, `${PYTHON} ${args.join(" ")}\n${done.stdout}${done.stderr}`);
}

/** Build the project in a new temporary directory, start the admin on a free port, and resolve with `{ dir, server, base }`. */
export async function startProject({ frame = false, embed = false } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "monorm-admin-smoke-"));
  // The built-in apps import from the source tree when `monorm` is an editable install; the root makes that path resolve.
  const env = { ...process.env, PYTHONPATH: ROOT, MONORM_DATABASE_URL: `sqlite://${dir}/app.db`, MONORM_ADMIN_SECRET: embed ? SECRET + "-identity" : SECRET, MONORM_ADMIN_PORT: "0" };
  writeFileSync(join(dir, "schema.prisma"), SCHEMA);
  writeFileSync(join(dir, "seed.py"), SEED);
  // The generated message classes of the proto field, and the map from the package to its module.
  cpSync(join(ROOT, "languages", "python", "tests", "proto_gen", "acme"), join(dir, "acme"), { recursive: true, filter: (f) => !f.includes("__pycache__") });
  writeFileSync(join(dir, "monorm.toml"), '[proto.python]\n"acme" = "acme.note_pb2"\n');
  run(dir, ["-m", "monorm", "admin", "init", "--models", "models"], env);
  // A search box on Book: the title is a plain column, so the search is "contains". A filter on its author.
  const module = readFileSync(join(dir, "admin.py"), "utf8");
  assert.ok(module.includes("admin.register(Book)"), "the generated admin.py registers Book");
  assert.ok(module.includes("admin.register(Note)"), "the generated admin.py registers Note");
  // Book: a section layout with a collapsed section. Note: a custom form from a module file.
  let book = 'admin.register(Book, {"list": {"search": [Book.title], "filters": [Book.author]}, "form": [{"title": "Basics", "fields": ["title", "created_at"]}, {"title": "More", "fields": ["author_id"], "collapsed": True}]})';
  if (frame) {
    book = book.replace('{"list":', '{"frames": [{"name": "identity", "title": "Identity health", "url": "/identity/admin/-/embed/book_health?id={id}", "height": "65vh"}], "list":');
  } else if (embed) {
    book = book.replace('{"list":', '{"fieldsets": [{"name": "health", "title": "Book health", "fields": ["title"]}], "list":');
  }
  writeFileSync(join(dir, "note-form.js"), NOTE_FORM);
  // Paper: a picker over Reviewer (with a create modal) and a file field in a memory store.
  assert.ok(module.includes("admin.register(Reviewer)") && module.includes("admin.register(Paper)"), "the generated admin.py registers Reviewer and Paper");
  assert.ok(module.includes("admin.register(Ticket)"), "the generated admin.py registers Ticket");
  assert.ok(module.includes("admin.register(Album)") && module.includes("admin.register(Track)"), "the generated admin.py registers Album and Track");
  assert.ok(module.includes("admin.register(Event)") && module.includes("admin.register(Gadget)"), "the generated admin.py registers Event and Gadget");
  assert.ok(module.includes("admin.register(Movie)"), "the generated admin.py registers Movie");
  assert.ok(module.includes("admin.register(Shelf)"), "the generated admin.py registers Shelf");
  assert.ok(module.includes("admin.register(Parcel)"), "the generated admin.py registers Parcel");
  // Movie: the example extension package (a stars widget, a Summary tab, a route and an action).
  cpSync(join(ROOT, "examples", "admin-package"), join(dir, "package"), { recursive: true });
  const options = ['storage=Registry({"media": AsyncOperator("memory")})'];
  if (frame) options.push(`frame_origins=["'self'"]`);
  if (embed) {
    options.push('base_path="/identity/admin"', 'brand={"name": "Identity"}');
    options.push(`embeds={"book_health": {"resource": "Book", "lookup": "id", "tab": "health", "ancestors": ["'self'"]}}`);
  }
  const withStorage = module
    .replace("from monorm.admin import Admin\n", "from monorm.admin import Admin, inline\nfrom monorm_file_storage import Registry\nfrom opendal import AsyncOperator\n")
    // Album: its Track rows as an inline that saves with the album.
    .replace("admin.register(Album)", 'admin.register(Album, {"inlines": [inline(Track)]})')
    .replace("admin = Admin()", `admin = Admin(${options.join(", ")})`)
    .replace("from monorm.admin import Admin, inline\n", "from monorm.admin import Admin, inline, action, message\n")
    .replace("admin.register(Ticket)", TICKET)
    .replace("admin.register(Movie)", "import sys\nsys.path.insert(0, str(__import__('pathlib').Path(__file__).parent / 'package'))\nfrom rating import install\n\ninstall(admin, Movie)")
    .replace("admin.register(Gadget)", 'admin.register(Gadget, {"widgets": {"Tier": "radio"}})')
    // Shelf: its cells edit in the list.
    .replace("admin.register(Shelf)", 'admin.register(Shelf, {"list": {"columns": ["title", "qty"], "editable": ["title", "qty"], "order": [Shelf.id]}})')
    // Parcel: a bool icon, badges (a value outside the map shows grey) and a date hierarchy.
    .replace("admin.register(Parcel)", 'admin.register(Parcel, {"list": {"columns": ["code", "placed", "paid", "state"], "order": [Parcel.id], "date_hierarchy": Parcel.placed, "badges": {Parcel.state: {"new": "blue", "shipped": "green", "lost": "red"}}}})')
    .replace("admin.register(Reviewer)", 'admin.register(Reviewer, {"list": {"search": [Reviewer.name]}})')
    .replace("admin.register(Track)", 'admin.register(Track, {"relations": {"reviewer": {}}})')
    .replace("admin.register(Paper)", 'admin.register(Paper, {"relations": {"reviewer": {"create": True}}, "files": {"cover": "media"}})');
  writeFileSync(join(dir, "admin.py"), withStorage.replace("admin.register(Book)", book).replace("admin.register(Note)", 'admin.register(Note, {"form_component": "note-form.js"})'));
  run(dir, ["-m", "monorm", "generate"], env);
  run(dir, ["-m", "monorm", "makemigrations"], env);
  run(dir, ["-m", "monorm", "migrate", "--url", env.MONORM_DATABASE_URL], env);
  run(dir, ["seed.py"], env);
  const server = spawn(PYTHON, ["-m", "monorm", "admin"], { cwd: dir, env, stdio: ["ignore", "ignore", "pipe"] });
  const base = await new Promise((resolveUrl, reject) => {
    let text = "";
    server.stderr.on("data", (chunk) => {
      text += chunk;
      const found = /"url":"([^"]+)"/.exec(text);
      if (found && text.includes('"listening"')) resolveUrl(found[1]);
    });
    server.on("exit", (code) => reject(new Error(`the admin ended with ${code}\n${text}`)));
  });

  return { dir, server, base };
}
