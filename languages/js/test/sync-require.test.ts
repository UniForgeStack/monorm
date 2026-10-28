/** A packaged runtime imports `monorm` and loads through a synchronous require(), so `monorm` must have no top-level await. */
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";

test("the monorm entry points load through a synchronous require", () => {
  const require = createRequire(import.meta.url);
  for (const entry of ["../src/index.js", "../src/ext.js"]) assert.ok(Object.keys(require(entry)).length > 0);
});
