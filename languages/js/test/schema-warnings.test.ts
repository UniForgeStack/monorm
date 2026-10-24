/** A plain `"..."` check warns through `process.emitWarning`; a native or `raw()` check does not. */
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { load, loads, Registry } from "../src/index.js";
import { native } from "../src/native.js";

const schema = (check: string) =>
  `datasource db { provider = "sqlite" }\nmodel Stock {\n  id Int @id\n  qty Int\n  @@check(${check})\n}\n`;

/** The warnings of type `SchemaWarning` that `run` emits (`process` emits them on a later tick). */
async function schemaWarnings(run: () => void): Promise<Error[]> {
  const seen: Error[] = [];
  const listener = (warning: Error) => {
    if (warning.name === "SchemaWarning") seen.push(warning);
  };
  process.on("warning", listener);
  try {
    run();
    await new Promise((resolve) => setImmediate(resolve));
  } finally {
    process.off("warning", listener);
  }
  return seen;
}

test("a plain string check warns with file, line and column, and names raw()", async () => {
  const dir = mkdtempSync(join(tmpdir(), "orm-warning-"));
  try {
    const file = join(dir, "schema.prisma");
    writeFileSync(file, schema('"qty > 0"'));
    const seen = await schemaWarnings(() => void load(file, { registry: new Registry() }));
    assert.equal(seen.length, 1);
    assert.match(seen[0]?.message ?? "", /schema\.prisma:5:11: warning: .*raw\(/);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("generateTypescript emits the schema warnings", async () => {
  const dir = mkdtempSync(join(tmpdir(), "orm-warning-"));
  try {
    const file = join(dir, "schema.prisma");
    writeFileSync(file, schema('"qty > 0"'));
    const seen = await schemaWarnings(() => void native().generateTypescript(file));
    assert.equal(seen.length, 1);
    assert.match(seen[0]?.message ?? "", /schema\.prisma:5:11: warning: .*raw\(/);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("a native or raw() check does not warn", async () => {
  const seen = await schemaWarnings(() => {
    loads(schema("qty > 0"), { registry: new Registry() });
    loads(schema('raw("qty > 0")'), { registry: new Registry() });
  });
  assert.deepEqual(seen, []);
});
