import assert from "node:assert/strict";
import { test } from "node:test";

import { cellsOf } from "../src/rows.js";

test("packed NULL columns and nested arrays decode once per row set", () => {
  // SQL NULL is the same wire value for every column type, including arrays
  // (which the model schema cannot declare nullable).
  const packed = Buffer.from([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    14, 3, 0, 0, 0, // array of NULL, empty array, and [true, NULL]
    0,
    14, 0, 0, 0, 0,
    14, 2, 0, 0, 0, 2, 0,
  ]);
  const rows = { n: 1, width: 13, packed };
  const values = cellsOf(rows);
  assert.deepEqual(values, [...Array(12).fill(null), [null, [], [true, null]]]);
  assert.equal(cellsOf(rows), values);
});
