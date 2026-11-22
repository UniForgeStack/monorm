// Admin date drill-down: the `dates` route and the `dateHierarchy` list key, on PostgreSQL and SQLite.
import assert from "node:assert/strict";
import { test } from "node:test";

import { Admin, AdminConfigError, type ListConfig } from "../src/admin.js";
import { connect, loads, param, Registry } from "../src/index.js";

const PG_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

const SOURCE = `
datasource db {
  provider = "%(provider)s"
}

model DtPerson {
  id     Int       @id
  name   String
  joined DateTime  @db.Date
  events DtEvent[]
  @@map("dt_people")
}

model DtEvent {
  id        Int      @id
  title     String
  person_id Int
  at        DateTime
  day       DateTime? @db.Date
  person    DtPerson @relation(fields: [person_id], references: [id])
  @@index([at])
  @@map("dt_events")
}
`;

const SU1 = { key: 1, is_superuser: true };
const SU2 = { key: 2, is_superuser: true };
const DATES = "/admin/api/dates/DtEvent";
const MONTHS = "2026-01-01T00:00:00Z,2026-02-01T00:00:00Z,2026-03-01T00:00:00Z,2026-04-01T00:00:00Z";
const PROVIDERS = ["sqlite", "postgres"] as const;

// Models are loose here: the columns are reached by name.
type Loose = any;
type Database = Awaited<ReturnType<typeof connect>>;
interface World {
  registry: Registry;
  db: Database;
  Person: Loose;
  Event: Loose;
}

async function world(provider: (typeof PROVIDERS)[number], run: (w: World) => Promise<void>): Promise<void> {
  const registry = new Registry();
  const models = loads(SOURCE.replace("%(provider)s", provider), { registry }) as Record<string, Loose>;
  const db = await connect(provider === "sqlite" ? "sqlite://:memory:" : PG_URL, { registry, default: false });
  try {
    await db.dropTables();
    await db.createTables();
    const Person = models["DtPerson"], Event = models["DtEvent"];
    const date = (y: number, m: number, d: number) => new Date(Date.UTC(y, m - 1, d));
    await Person.objects.using(db).insertMany([
      { id: 1, name: "Ada", joined: date(2025, 12, 31) },
      { id: 2, name: "Bo", joined: date(2026, 1, 15) },
    ]);
    const at = (y: number, m: number, d: number, h = 0, min = 0, s = 0) => new Date(Date.UTC(y, m - 1, d, h, min, s));
    await Event.objects.using(db).insertMany([
      { id: 1, title: "one", personId: 1, at: at(2026, 1, 2, 3, 4, 5), day: date(2026, 1, 2) },
      { id: 2, title: "two", personId: 1, at: at(2026, 2, 1), day: date(2026, 2, 1) },
      { id: 3, title: "three", personId: 2, at: at(2026, 3, 1), day: null },
      { id: 4, title: "four", personId: 1, at: at(2026, 3, 15, 12), day: date(2026, 3, 15) },
      { id: 5, title: "five", personId: 1, at: at(2026, 3, 31, 22, 30), day: date(2026, 3, 31) },
    ]);
    await run({ registry, db, Person, Event });
  } finally {
    await db.dropTables();
    await db.close();
  }
}

function clientOf(w: World, hierarchy?: unknown, user: Record<string, unknown> = SU1, listOptions: ListConfig = {}) {
  const E = w.Event;
  const site = new Admin(w.registry);
  const config = { columns: [E.title], order: [E.id], search: [E.title], dateHierarchy: hierarchy ?? E.at, ...listOptions };
  site.register(E, { list: config as ListConfig });
  site.register(w.Person);
  return { site, client: site.testClient(w.db, { user }) };
}

const instant = (text: string) => new Date(text.replace("Z", "+00:00")).getTime();
const resourceOf = async (client: ReturnType<typeof clientOf>["client"], name: string) =>
  (await client.get("/admin/api/resources")).json().resources.find((r: { name: string }) => r.name === name);

for (const provider of PROVIDERS) {
  test(`the range is one statement and follows the filters and the search (${provider})`, () => world(provider, async (w) => {
    const { client } = clientOf(w);
    const preamble = (await client.get("/admin/api/resources")).sql.length;
    const reply = await client.get(DATES);
    assert.equal(reply.status, 200, JSON.stringify(reply.json()));
    assert.equal(reply.sql.length, preamble + 1);
    const body = reply.json();
    assert.equal(instant(body.min), Date.UTC(2026, 0, 2, 3, 4, 5));
    assert.equal(instant(body.max), Date.UTC(2026, 2, 31, 22, 30));
    const searched = (await client.get(`${DATES}?q=th`)).json();
    assert.equal(instant(searched.min), Date.UTC(2026, 2, 1), "the search narrows the range");
    assert.equal(instant(searched.max), Date.UTC(2026, 2, 1));
    assert.equal(instant((await client.get(`${DATES}?f.at:gte=2026-02-01T00:00:00Z`)).json().min), Date.UTC(2026, 1, 1));
    const nothing = (await client.get(`${DATES}?q=nothing`)).json();
    assert.deepEqual([nothing.min, nothing.max], [null, null]);
  }));

  test(`counts per month use the edges of the browser zone and one statement (${provider})`, () => world(provider, async (w) => {
    const { client } = clientOf(w);
    const preamble = (await client.get("/admin/api/resources")).sql.length;
    const reply = await client.get(`${DATES}?edges=${MONTHS}`);
    assert.equal(reply.status, 200, JSON.stringify(reply.json()));
    assert.deepEqual(reply.json().counts, [1, 1, 3]);
    assert.equal(reply.sql.length, preamble + 1);
    const local = "2026-03-01T00:00:00%2B02:00,2026-04-01T00:00:00%2B02:00,2026-05-01T00:00:00%2B02:00";
    const zoned = await client.get(`${DATES}?edges=${local}`);
    assert.deepEqual(zoned.json().counts, [2, 1], "the row at 22:30Z on March 31 is April 1 at +02:00");
    assert.equal(zoned.sql.length, preamble + 1);
    assert.deepEqual((await client.get(`${DATES}?edges=${MONTHS}&q=o`)).json().counts, [1, 1, 1], "the search applies");
  }));

  test(`counts of a date field use plain dates and a null is in no period (${provider})`, () => world(provider, async (w) => {
    const { client } = clientOf(w, w.Event.day);
    const reply = await client.get(`${DATES}?edges=2026-01-01,2026-02-01,2026-03-01,2026-04-01`);
    assert.deepEqual(reply.json().counts, [1, 1, 2], "row 3 has no day");
    const body = (await client.get(DATES)).json();
    assert.deepEqual([body.min, body.max], ["2026-01-02", "2026-03-31"]);
  }));

  test(`a bad edge list answers 400 (${provider})`, () => world(provider, async (w) => {
    const { client } = clientOf(w);
    const years = Array.from({ length: 62 }, (_, i) => `${1990 + i}-01-01T00:00:00Z`);
    const cases: Record<string, string> = {
      "equal edges": "2026-01-01T00:00:00Z,2026-01-01T00:00:00Z",
      "falling edges": "2026-02-01T00:00:00Z,2026-01-01T00:00:00Z",
      "one edge": "2026-01-01T00:00:00Z",
      "a bad instant": "2026-01-01T00:00:00Z,yesterday",
      "a date without a time": "2026-01-01,2026-02-01",
      "62 edges": years.join(","),
    };
    for (const [what, edges] of Object.entries(cases)) {
      assert.equal((await client.get(`${DATES}?edges=${edges}`)).status, 400, what);
    }
    assert.equal((await client.get(`${DATES}?edges=${years.slice(0, 61).join(",")}`)).status, 200, "61 edges are the most");
    const byDay = clientOf(w, w.Event.day).client;
    assert.equal((await byDay.get(`${DATES}?edges=2026-01-01T00:00:00Z,2026-02-01T00:00:00Z`)).status, 400, "an instant for a date field");
  }));

  test(`a resource without date hierarchy answers 404 (${provider})`, () => world(provider, async (w) => {
    const site = new Admin(w.registry);
    site.register(w.Event, { list: { columns: [w.Event.title] } });
    site.register(w.Person);
    assert.equal((await site.testClient(w.db, { user: SU1 }).get(DATES)).status, 404);
  }));

  test(`the row scope limits the range and the counts (${provider})`, () => world(provider, async (w) => {
    const E = w.Event;
    const scoped = E.objects.filter(E.personId.eq(param("user"))).prepare();
    const owner2 = clientOf(w, undefined, SU2).client;
    const site = new Admin(w.registry);
    site.register(E, { queryset: scoped, list: { columns: [E.title], order: [E.id], dateHierarchy: E.at } });
    site.register(w.Person);
    const client = site.testClient(w.db, { user: SU2 });
    assert.deepEqual((await client.get(`${DATES}?edges=${MONTHS}`)).json().counts, [0, 0, 1], "person 2 holds one row");
    const body = (await client.get(DATES)).json();
    assert.equal(instant(body.min), Date.UTC(2026, 2, 1));
    assert.equal(instant(body.max), Date.UTC(2026, 2, 1));
    assert.deepEqual((await owner2.get(`${DATES}?edges=${MONTHS}`)).json().counts, [1, 1, 3], "without the queryset: every row");
  }));

  test(`a field of a to one relation counts the rows of the list (${provider})`, () => world(provider, async (w) => {
    const { client } = clientOf(w, w.Event.person.joined);
    const reply = await client.get(`${DATES}?edges=2025-12-01,2026-01-01,2026-02-01`);
    assert.equal(reply.status, 200, JSON.stringify(reply.json()));
    assert.deepEqual(reply.json().counts, [4, 1], "four events of Ada (joined in December) and one of Bo");
    const meta = await resourceOf(client, "DtEvent");
    assert.deepEqual(meta.date_hierarchy, { field: "person.joined", counts: true, kind: "date" });
    assert.ok(meta.filters.some((f: { name: string }) => f.name === "person.joined"), "the field joins the filters");
  }));

  test(`counts false reaches the metadata and the field joins the filters (${provider})`, () => world(provider, async (w) => {
    const E = w.Event;
    const { client } = clientOf(w, { field: E.at, counts: false });
    const meta = await resourceOf(client, "DtEvent");
    assert.deepEqual(meta.date_hierarchy, { field: "at", counts: false, kind: "datetime" });
    assert.deepEqual(meta.filters.map((f: { name: string }) => f.name), ["at"]);
    const plain = clientOf(w, undefined, SU1, { filters: [E.day] }).client;
    const listed = await resourceOf(plain, "DtEvent");
    assert.deepEqual(listed.filters.map((f: { name: string }) => f.name), ["day", "at"], "a listed filter stays and the date field follows");
  }));

  test(`a date hierarchy that does not fit is a build error or a warning (${provider})`, () => world(provider, async (w) => {
    const E = w.Event;
    const cases: [unknown, RegExp][] = [
      [E.title, /takes a date or datetime/],
      ["nope", /date_hierarchy/],
      [{ counts: true }, /needs a field/],
    ];
    for (const [hierarchy, text] of cases) {
      const site = new Admin(w.registry);
      site.register(E, { list: { columns: [E.title], dateHierarchy: hierarchy } as ListConfig });
      site.register(w.Person);
      assert.throws(() => site.check(), (e) => e instanceof AdminConfigError && text.test(e.message));
    }
    const dated = (field: unknown) => clientOf(w, field).site.check().filter((m: string) => m.includes("date_hierarchy"));
    assert.deepEqual(dated(E.at), [], "`at` leads an index");
    const warnings = dated(E.day);
    assert.equal(warnings.length, 1);
    assert.ok(warnings[0]!.includes("DtEvent") && warnings[0]!.includes("day") && warnings[0]!.includes("index"));
  }));
}
