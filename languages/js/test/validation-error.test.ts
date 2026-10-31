/** `ValidationError`: messages per field path, and messages with no field. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, ORMError, Registry, ValidationError } from "../src/index.js";
import * as index from "../src/index.js";
import { call, native } from "../src/native.js";

const SCHEMA = `datasource db { provider = "sqlite" }
model Doc {
  id   Int @id
  body Json?
}`;

// [fields, messages with no field, the display text]: the same table is in the Python twin.
const DISPLAY_TEXTS: [Record<string, string[]>, string[], string][] = [
  [{}, [], "validation failed"],
  [{}, ["m1", "m2"], "m1; m2"],
  [{ a: ["x", "y"], "b.2.c": ["z"] }, [], "a: x; a: y; b.2.c: z"],
  [{ a: ["x"] }, ["m"], "m; a: x"],
];

test("a field path addresses a nested field and a list item", () => {
  const error = new ValidationError({ "address.city": "is required" });
  error.add(["lines", 2, "qty"], "must be positive").add("lines.2.qty", "is too large");
  assert.deepEqual([...error.fields], [
    ["address.city", ["is required"]],
    ["lines.2.qty", ["must be positive", "is too large"]],
  ]);
  assert.deepEqual(error.nonFieldMessages, []);
});

test("a field takes one message or a list", () => {
  const error = new ValidationError({ status: "is locked", name: ["is empty", "is short"] });
  assert.deepEqual([...error.fields], [["status", ["is locked"]], ["name", ["is empty", "is short"]]]);
});

test("messages with no field stay apart from the fields", () => {
  const error = new ValidationError("The order is locked.");
  error.addNonFieldMessage("Try again later.");
  assert.equal(error.fields.size, 0);
  assert.deepEqual(error.nonFieldMessages, ["The order is locked.", "Try again later."]);
  const both = new ValidationError({ a: "x" }, { nonFieldMessages: ["m"] });
  assert.deepEqual([...both.fields], [["a", ["x"]]]);
  assert.deepEqual(both.nonFieldMessages, ["m"]);
  assert.deepEqual(JSON.parse(JSON.stringify(both)), { name: "ValidationError", message: both.message, fields: { a: ["x"] }, nonFieldMessages: ["m"] });
});

test("the text lists the messages with no field first", () => {
  const error = new ValidationError({ "address.city": "is required" }, { nonFieldMessages: ["The order is locked."] });
  assert.equal(error.message, "The order is locked.; address.city: is required");
  assert.equal(new ValidationError().message, "validation failed");
  assert.equal(error.add("b", "x").message, "The order is locked.; address.city: is required; b: x");
});

test("the display text is the same in Rust and the language", () => {
  for (const [fields, nonFieldMessages, text] of DISPLAY_TEXTS) {
    const error = new ValidationError(fields, { nonFieldMessages });
    assert.equal(error.message, text);
    assert.equal(native()._testValidationText(error), text);
  }
});

test("paths keep the order of first use whatever their names", () => {
  const paths = ["b", "constructor", "2", "__proto__", "a", "0", "toString"];
  const error = new ValidationError();
  for (const path of paths) error.add(path, "x");
  error.add("constructor", "y");
  assert.deepEqual([...error.fields.keys()], paths);
  assert.deepEqual(error.fields.get("constructor"), ["x", "y"]);
  assert.throws(
    () => call(() => native()._testRaiseValidationError(error)),
    (raised: unknown) => {
      assert.ok(raised instanceof ValidationError && raised !== error);
      assert.deepEqual([...raised.fields.keys()], paths);
      assert.deepEqual(raised.fields.get("constructor"), ["x", "y"]);
      return true;
    },
  );
});

test("it is an ORM error that the package exports", () => {
  assert.ok(new ValidationError() instanceof ORMError);
  assert.equal(index.ValidationError, ValidationError);
});

test("rust reads the paths and messages and raises them back", () => {
  const original = new ValidationError({ b: "x", "lines.2.qty": ["y", "z"], a: "w" }, { nonFieldMessages: ["m"] });
  assert.throws(
    () => call(() => native()._testRaiseValidationError(original)),
    (raised: unknown) => {
      assert.ok(raised instanceof ValidationError && raised !== original);
      assert.deepEqual([...raised.fields], [["b", ["x"]], ["lines.2.qty", ["y", "z"]], ["a", ["w"]]]);
      assert.deepEqual(raised.nonFieldMessages, ["m"]);
      assert.equal(raised.message, original.message);
      return true;
    },
  );
  const other = new Error("not a validation error");
  assert.throws(
    () => call(() => native()._testRaiseValidationError(other)),
    (raised: unknown) => {
      assert.equal(raised, other);
      return true;
    },
  );
});

class Rejected extends ValidationError {}

test("an error that user code raises during conversion comes back unchanged", async () => {
  const registry = new Registry();
  const models = loads(SCHEMA, { registry }) as Record<string, any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const error = new Rejected({ body: "is odd" }, { nonFieldMessages: ["m"] });
    const unreadable = { toJSON() { throw error; } };
    const objects = models["Doc"].objects.using(db);
    await objects.insert({ id: 1 });
    const runs = [
      () => objects.insert({ id: 2, body: unreadable }),
      () => objects.filter(models["Doc"].id.eq(1)).update({ body: unreadable }),
    ];
    for (const run of runs) {
      await assert.rejects(run(), (caught: unknown) => {
        assert.equal(caught, error);
        return true;
      });
    }
  } finally {
    await db.close();
  }
});
