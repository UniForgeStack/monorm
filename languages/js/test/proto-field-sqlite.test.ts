/** The SQLite run of `proto-field.test.ts`: the generated models of a process carry one dialect, so it runs in a child process. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

test("the proto field cases pass on SQLite", () => {
  const file = fileURLToPath(new URL("./proto-field.test.js", import.meta.url));
  // A test runner child must not inherit the context of this one, or it would run as a plain child.
  const { NODE_TEST_CONTEXT: _context, ...env } = process.env;
  const run = spawnSync(process.execPath, ("Bun" in globalThis ? ["test", "--timeout", "300000", file] : ["--expose-gc", "--test", "--test-concurrency=1", "--test-reporter=spec", file]), { encoding: "utf8", env: { ...env, MONORM_PROTO_PROVIDER: "sqlite" } });
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
  assert.match(`${run.stdout}${run.stderr}`, /ℹ pass [1-9]|[1-9]\d* pass/);
});
