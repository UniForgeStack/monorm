/** Explicit query-set maps on the native generator match CLI output. */
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { native } from "../src/native.js";

test("native generation accepts query-set maps and matches the CLI", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-native-codegen-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const schema = join(dir, "schema.prisma");
  const output = join(dir, "models.ts");
  writeFileSync(schema, "model Post {\n id Int @id\n}\n");
  const args = ["--schema", schema, "generate", "typescript", "-o", output];
  const addon = native();
  assert.equal(await addon.cli(args), 0);
  const baseline = readFileSync(output, "utf8");
  assert.equal(addon.generateTypescript(schema), baseline);
  assert.equal(addon.generateTypescript(schema, null, null), baseline);
  assert.equal(addon.generateTypescript(schema, undefined, {}), baseline);

  assert.equal(await addon.cli([...args, "--import", "../runtime.js", "--query-set", "Post=./queries.js#PostQueries"]), 0);
  const generated = addon.generateTypescript(schema, "../runtime.js", { Post: "./queries.js#PostQueries" });
  assert.equal(generated, readFileSync(output, "utf8"));
  assert.ok(generated.includes('import * as _q0 from "./queries.js";'));
  assert.ok(generated.includes("useQuerySet(Post, () => _q0.PostQueries);"));
  assert.ok(generated.includes("readonly queries: _q0.PostQueries;"));

  assert.throws(() => addon.generateTypescript(schema, null, { Missing: "./queries.js#PostQueries" }), /no model Missing/);
  assert.throws(() => addon.generateTypescript(schema, null, { Post: "app.queries:PostQueries" }), /specifier#Export/);
});

test("native generation accepts subpath imports and dollar export names", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "monorm-native-codegen-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const schema = join(dir, "schema.prisma");
  writeFileSync(schema, "model Post {\n id Int @id\n}\n");
  const generated = native().generateTypescript(schema, null, { Post: "#app/queries#$Q$" });
  assert.ok(generated.includes('import * as _q0 from "#app/queries";'));
  assert.ok(generated.includes("useQuerySet(Post, () => _q0.$Q$);"));
  assert.ok(generated.includes("readonly queries: _q0.$Q$;"));
});
