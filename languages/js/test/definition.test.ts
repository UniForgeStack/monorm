import assert from "node:assert/strict";
import { test } from "node:test";
import { Registry, define, loads, SchemaError } from "../src/index.js";

const good = `model User {
  id Int @id
  name String
}`;

test("definition is eager and failure is atomic", () => {
  const registry = new Registry();
  const User = loads(good, { registry })["User"]!;
  const prepared = registry.native();
  const before = registry.ir();
  assert.throws(() => define({ models: [{ name: "Bad", table: "bad", fields: [
    { name: "id", column: "id", type: "int", primary_key: true },
  ], relations: [{ name: "missing", kind: "one", target: "Missing", from: "id", to: "id" }] }] }, { registry }), SchemaError);
  assert.deepEqual(registry.ir(), before);
  assert.equal(registry.native(), prepared);
  assert.equal(registry.get("User").model, User);
});

test("existing models keep their prepared snapshot", () => {
  const registry = new Registry();
  const User = loads(good, { registry })["User"]!;
  const snapshot = User._meta.registry.native();
  loads(`model Other {\n id Int @id\n}`, { registry });
  assert.equal(User._meta.registry.native(), snapshot);
  assert.notEqual(registry.native(), snapshot);
});

test("definition owns input schema", () => {
  const registry = new Registry();
  const ir = { models: [{ name: "User", table: "users", fields: [{ name: "id", column: "id", type: "int", primary_key: true }], relations: [] }] };
  const User = define(ir as never, { registry })["User"]!;
  ir.models[0]!.table = "mutated";
  assert.equal((User._meta.registry.ir() as unknown as { models: { table: string }[] }).models[0]!.table, "users");
});
