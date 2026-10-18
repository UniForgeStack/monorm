import assert from "node:assert/strict";
import { test } from "node:test";
import { hardDeleteStatement, LANGUAGE_CONTRACT } from "../src/ext.js";
import { User } from "./blog/models.js";

test("the hard-delete statement is a delete of the query set", () => {
  const qs = User.objects.filter(User.id.eq(1n));
  assert.deepEqual(hardDeleteStatement(qs).inspect(), hardDeleteStatement(qs, { returning: false }).inspect());
  assert.equal(LANGUAGE_CONTRACT, 1);
});
