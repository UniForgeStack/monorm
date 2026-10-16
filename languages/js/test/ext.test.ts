import assert from "node:assert/strict";
import { test } from "node:test";
import { hardDeleteStatement, LANGUAGE_CONTRACT } from "../src/ext.js";
import { User } from "./blog/models.js";

test("the hard-delete statement matches the query-set method", () => {
  const qs = User.objects.filter(User.id.eq(1n));
  assert.deepEqual(hardDeleteStatement(qs).inspect(), qs.hardDelete().inspect());
  assert.equal(LANGUAGE_CONTRACT, 1);
});
