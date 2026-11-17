/** The `proto` extension: `@proto.message("pkg.Msg")` on a `Bytes` field, end to end on PostgreSQL and SQLite. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { create, toBinary } from "@bufbuild/protobuf";
import { connect, loads, QueryError, Registry, SchemaError, func } from "../src/index.js";
import { ProtoError, protoCodec } from "monorm/extensions/proto";
import type * as pgEs from "./proto/es/models.js";
import type * as pgTs from "./proto/tsproto/models.js";
import type * as pgCustom from "./proto/custom/models.js";
import { InvoiceSchema } from "./proto/gen/acme/billing/invoice_pb.js";
import { NoteSchema, NoteV2Schema, OtherSchema, Outer_InnerSchema, OuterSchema } from "./proto/gen/acme/note_pb.js";
import { Note as TsNote, NoteV2 as TsNoteV2 } from "./proto/gen-ts-proto/acme/note.js";
import { Plain, Served } from "./proto/custom/thing.js";
import { scratch } from "./scratch.js";

const FIXTURES = join(import.meta.dirname, "..", "..", "test", "proto");
const CLI = fileURLToPath(new URL("../src/cli.js", import.meta.url));

const note = (title = "first", priority = 3, tags: string[] = ["a", "b"], label?: string) =>
  create(NoteSchema, { title, priority, tags, ...(label === undefined ? {} : { inner: create(Outer_InnerSchema, { label }) }) });
const encode = (message: ReturnType<typeof note>) => toBinary(NoteSchema, message);

// A generated model carries its dialect and the default registry takes one dialect, so one process runs one provider:
// the run for SQLite is `proto-field-sqlite.test.ts`, which starts this file with the variable set.
const PROVIDER = process.env["MONORM_PROTO_PROVIDER"] === "sqlite" ? "sqlite" : "postgresql";
const suffix = PROVIDER === "sqlite" ? ".sqlite" : "";
const { ProtoEvent } = (await import(`./proto/es/models${suffix}.js`)) as typeof pgEs;
const { TsProtoEvent } = (await import(`./proto/tsproto/models${suffix}.js`)) as typeof pgTs;
const { CustomEvent } = (await import(`./proto/custom/models${suffix}.js`)) as typeof pgCustom;

for (const provider of [PROVIDER]) {
  const url = provider === "sqlite" ? "sqlite://:memory:" : (process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test");

  async function withDatabase(t: { skip(why: string): void }, run: (db: any) => Promise<void>): Promise<void> {
    let db: any;
    try {
      db = await connect(url, { default: false });
    } catch (error) {
      if (process.env["ORM_TEST_DATABASE_URL"] !== undefined) throw error;
      t.skip("no reachable database");
      return;
    }
    try {
      await db.dropTables();
      await db.createTables();
      await run(db);
    } finally {
      await db.dropTables();
      await db.close();
    }
  }

  const stored = async (db: any, id = 1, column = "payload"): Promise<Uint8Array | null> => {
    const row = (await db.fetch(`SELECT "${column}" AS v FROM proto_event WHERE id = ${id}`))[0];
    return row.v === null ? null : new Uint8Array(row.v);
  };

  test(`proto round trip (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      const row = await qs.insert({ id: 1, payload: note("first", 3, ["a", "b"], "deep"), inner: create(Outer_InnerSchema, { label: "in" }), bill: create(InvoiceSchema, { number: "n-1", cents: 250n }) });
      assert.equal(row.payload?.title, "first");
      const loaded = (await qs.get(ProtoEvent.id.eq(1)))!;
      assert.deepEqual(loaded.payload, note("first", 3, ["a", "b"], "deep"));
      assert.equal(loaded.inner?.label, "in");
      assert.equal(loaded.bill?.number, "n-1");
      assert.equal(loaded.bill?.cents, 250n);
      assert.equal(loaded.raw, null);
      assert.deepEqual(await stored(db), encode(note("first", 3, ["a", "b"], "deep")));
    });
  });

  test(`a null and an empty message differ (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      await qs.insertMany([{ id: 1, payload: null }, { id: 2, payload: create(NoteSchema) }]);
      const rows = new Map((await qs.orderBy(ProtoEvent.id).all()).map((r) => [r.id, r.payload]));
      assert.equal(rows.get(1), null);
      assert.deepEqual(rows.get(2), create(NoteSchema));
      assert.equal((await stored(db, 2))?.length, 0);
    });
  });

  test(`bulk writes encode every row (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      await qs.insertMany([1, 2, 3, 4, 5].map((i) => ({ id: i, payload: note(`t${i}`, i) })));
      assert.deepEqual((await qs.orderBy(ProtoEvent.id).all()).map((r) => r.payload?.title), ["t1", "t2", "t3", "t4", "t5"]);
      await qs.filter(ProtoEvent.id.gt(3)).update({ payload: note("late") });
      await qs.filter(ProtoEvent.id.eq(1)).update({ payload: null });
      const got = new Map((await qs.orderBy(ProtoEvent.id).all()).map((r) => [r.id, r.payload]));
      assert.equal(got.get(1), null);
      assert.equal(got.get(2)?.title, "t2");
      assert.equal(got.get(4)?.title, "late");
      assert.equal(got.get(5)?.title, "late");
      await qs.updateMany([{ id: 2, payload: note("two") }, { id: 3, payload: note("three") }]);
      assert.deepEqual((await qs.filter(ProtoEvent.id.in([2, 3])).orderBy(ProtoEvent.id).all()).map((r) => r.payload?.title), ["two", "three"]);
    });
  });

  test(`reading a row decodes the field once when the row loads (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      await qs.insert({ id: 1, payload: note("x") });
      const row = (await qs.get(ProtoEvent.id.eq(1)))!;
      // The loaded message is an object of the row, so a change to it stays on the row; the stored bytes do not change.
      (row.payload as { title: string }).title = "kept";
      assert.equal(row.payload?.title, "kept");
      assert.equal((await qs.get(ProtoEvent.id.eq(1)))!.payload?.title, "x");
    });
  });

  test(`a message of the wrong class is refused before the statement (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      const bad: unknown[] = [create(OtherSchema, { name: "x" }), create(InvoiceSchema, { number: "n" }), Uint8Array.of(0x0a, 0x01, 0x61), "text", 7, { title: "plain object" }];
      for (const value of bad) await assert.rejects(async () => qs.insert({ id: 1, payload: value as never }), (e: Error) => e instanceof ProtoError && /acme\.Note/.test(e.message));
      assert.equal(await qs.count(), 0);
      await assert.rejects(async () => qs.insertMany([{ id: 1, payload: note() }, { id: 2, payload: create(OtherSchema) as never }]), ProtoError);
      assert.equal(await qs.count(), 0);
    });
  });

  test(`a nested message class is found by its dotted name (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      await assert.rejects(async () => qs.insert({ id: 1, inner: create(OuterSchema) as never }), (e: Error) => e instanceof ProtoError && /Outer\.Inner/.test(e.message));
      await qs.insert({ id: 1, inner: create(Outer_InnerSchema, { label: "ok" }) });
      assert.equal((await qs.get(ProtoEvent.id.eq(1)))!.inner?.label, "ok");
    });
  });

  test(`is null tests work and every other use is refused (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      await qs.insertMany([{ id: 1, payload: note() }, { id: 2, payload: null }]);
      assert.deepEqual((await qs.filter(ProtoEvent.payload.isNull()).select({ id: ProtoEvent.id }).all()).map((r) => r.id), [2]);
      assert.deepEqual((await qs.filter(ProtoEvent.payload.isNotNull()).select({ id: ProtoEvent.id }).all()).map((r) => r.id), [1]);
      const use = /IS NULL or IS NOT NULL/;
      const refused: [() => unknown, RegExp][] = [
        [() => qs.filter(ProtoEvent.payload.eq(note() as never)), use],
        [() => qs.filter(ProtoEvent.payload.ne(note() as never)), /nullable/],
        [() => qs.filter(ProtoEvent.payload.in([note()] as never)), use],
        [() => qs.orderBy(ProtoEvent.payload), /not defined on a Bytes value/],
        [() => qs.select({ p: ProtoEvent.payload, n: func.length(ProtoEvent.payload as never) }), use],
      ];
      for (const [build, match] of refused) await assert.rejects(Promise.resolve().then(async () => await (build() as any)), (e: Error) => e instanceof QueryError && match.test(e.message), String(match));
      // Selecting the whole field is allowed and gives the stored bytes, with no decoding.
      const whole = (await qs.orderBy(ProtoEvent.id).select({ p: ProtoEvent.payload }).all()).map((r) => (r.p === null ? null : Buffer.from(r.p as unknown as Uint8Array).toString("hex")));
      assert.deepEqual(whole, [Buffer.from(encode(note())).toString("hex"), null]);
      await assert.rejects(Promise.resolve().then(async () => await qs.select({ m: func.max(ProtoEvent.payload as never) }).all()), (e: Error) => e instanceof QueryError && /not defined on a Bytes value/.test(e.message));
    });
  });

  test(`unknown fields of a later release survive a round trip (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = ProtoEvent.objects.using(db);
      const later = toBinary(NoteV2Schema, create(NoteV2Schema, { title: "n", priority: 1, tags: ["x"], addedLater: "kept" }));
      await qs.insert({ id: 1, raw: later });
      await db.execute("UPDATE proto_event SET payload = raw");
      const row = (await qs.get(ProtoEvent.id.eq(1)))!;
      assert.equal(row.payload?.title, "n");
      await qs.filter(ProtoEvent.id.eq(1)).update({ payload: { ...row.payload!, title: "edited" } as never });
      const again = await stored(db);
      assert.ok(again);
      // protobuf-es keeps the unknown field; the new bytes still hold field 5.
      const holds = Buffer.from(again).includes(Buffer.from("kept"));
      assert.ok(holds, "protobuf-es keeps unknown fields of a message across decode and encode");
    });
  });

  test(`a ts-proto class round trips and refuses nothing it cannot check (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = TsProtoEvent.objects.using(db);
      const value = TsNote.fromPartial({ title: "ts", priority: 2, tags: ["p"] });
      await qs.insertMany([{ id: 1, body: value }, { id: 2, body: null }]);
      const rows = await qs.orderBy(TsProtoEvent.id).all();
      assert.equal(rows[0]!.body?.title, "ts");
      assert.deepEqual(rows[0]!.body?.tags, ["p"]);
      assert.equal(rows[1]!.body, null);
      const raw = (await db.fetch("SELECT body AS v FROM ts_proto_event WHERE id = 1"))[0].v;
      assert.deepEqual(TsNote.decode(new Uint8Array(raw)), TsNote.fromPartial({ title: "ts", priority: 2, tags: ["p"] }));
      // ts-proto exports an interface and a const object: there is no class to test, so a value of another shape is encoded as it is.
      const later = TsNoteV2.encode(TsNoteV2.fromPartial({ title: "n", addedLater: "kept" })).finish();
      assert.ok(TsNote.decode(later).title === "n");
    });
  });

  test(`a registered codec serves a class without a protobuf api (${provider})`, async (t) => {
    await withDatabase(t, async (db) => {
      const qs = CustomEvent.objects.using(db);
      await assert.rejects(async () => qs.insert({ id: 9, plain: new Plain("a") }), (e: Error) => e instanceof ProtoError && /protoCodec/.test(e.message));
      assert.equal(await qs.count(), 0);
      protoCodec(Served, { encode: (m) => new TextEncoder().encode((m as Served).text), decode: (b) => new Served(new TextDecoder().decode(b)) });
      await qs.insertMany([{ id: 1, served: new Served("hello") }, { id: 2, served: null }]);
      assert.deepEqual((await qs.orderBy(CustomEvent.id).all()).map((r) => r.served?.text ?? null), ["hello", null]);
      await assert.rejects(async () => qs.insert({ id: 3, served: "hello" as never }), (e: Error) => e instanceof ProtoError && /fake\.Served/.test(e.message));
    });
  });
}

const refusedBy = (schema: string, match: RegExp) => assert.throws(() => loads(`datasource db {\n  provider = "sqlite"\n}\n${schema}`, { registry: new Registry() }), (e: Error) => e instanceof SchemaError && match.test(e.message), String(match));

test("a proto field that the database could not use is refused by the schema", () => {
  const cases: [string, string, RegExp][] = [
    ['body Bytes? @proto.message("a.B") @index', "", /@index/],
    ['body Bytes? @proto.message("a.B") @unique', "", /@unique/],
    ['body Bytes? @proto.message("a.B") @check("body IS NOT NULL")', "", /@check/],
    ['body Bytes? @proto.message("a.B") @default(raw("x\'00\'"))', "", /default/],
    ['body Bytes[] @proto.message("a.B")', "", /Bytes\[\]/],
    ['body String? @proto.message("a.B")', "", /Bytes field is required/],
    ['body Bytes? @proto.message("Note")', "", /full message name/],
    ['body Bytes? @proto.message("a.B")', "@@index([body])", /@index/],
  ];
  for (const [fields, extra, match] of cases) refusedBy(`model M {\n id Int @id\n ${fields}\n ${extra}\n}`, match);
});

test("a proto field cannot be a key or sit in a nested type", () => {
  refusedBy('model M {\n body Bytes @id @proto.message("a.B")\n}', /key/);
  refusedBy('type T {\n  body Bytes? @proto.message("a.B")\n}\nmodel M {\n id Int @id\n t T\n}', /unknown attribute/);
});

test("a message without a package map entry is a schema error", () => {
  const root = scratch("proto-nomap-");
  mkdirSync(root, { recursive: true });
  writeFileSync(join(root, "schema.prisma"), readFileSync(join(FIXTURES, "es", "schema.prisma"), "utf8"));
  writeFileSync(join(root, "monorm.toml"), '[proto.typescript]\n"other" = "./other_pb.js"\n');
  const run = spawnSync(process.execPath, [CLI, "--schema", "schema.prisma", "generate", "typescript", "-o", "models.ts", "--import", "monorm"], { cwd: root, encoding: "utf8" });
  assert.notEqual(run.status, 0);
  assert.match(run.stderr, /acme\.Note/);
  assert.match(run.stderr, /proto\.typescript/);
});

test("the generated proto models are current", () => {
  const root = scratch("proto-gen-");
  for (const dir of ["es", "tsproto", "custom"]) {
    for (const [schema, models] of [["schema.prisma", "models.ts"], ["schema.sqlite.prisma", "models.sqlite.ts"]] as const) {
      const out = join(root, `${dir}-${models}`);
      const run = spawnSync(process.execPath, [CLI, "--schema", schema, "generate", "typescript", "-o", out, "--import", "../../../src/index.js"], { cwd: join(FIXTURES, dir), encoding: "utf8" });
      assert.equal(run.status, 0, run.stderr);
      assert.equal(readFileSync(out, "utf8"), readFileSync(join(FIXTURES, dir, models), "utf8"), `run \`monorm generate typescript\` in test/proto/${dir} for ${schema}`);
    }
  }
});
