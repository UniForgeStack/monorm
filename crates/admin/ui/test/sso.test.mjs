// Unit tests of the sign-in with no form (src/sso.ts): when the page goes to the provider, and the address it sends.
import assert from "node:assert/strict";
import { test } from "node:test";

import { LOOP_WINDOW_MS, shouldAutoStart, startUrl, wantsForm } from "../src/sso.ts";

const visitor = { sso: "/admin/auth/start", autoStart: true, formOnly: false, lastStart: null, now: 1_000_000 };

test("a visitor with no session goes to the provider only when the site sets auto_start", () => {
  assert.equal(shouldAutoStart(visitor), true);
  assert.equal(shouldAutoStart({ ...visitor, autoStart: false }), false);
  assert.equal(shouldAutoStart({ ...visitor, sso: null }), false);
});

test("a failed callback and the break-glass address show the form", () => {
  assert.equal(shouldAutoStart({ ...visitor, formOnly: true }), false);
  assert.equal(wantsForm("?sso_error=denied"), true);
  assert.equal(wantsForm("?password=1"), true);
  assert.equal(wantsForm("?password=0"), false);
  assert.equal(wantsForm("?page=2"), false);
  assert.equal(wantsForm(""), false);
});

test("a start that gave no session shows the form for a while, so the page never loops", () => {
  assert.equal(shouldAutoStart({ ...visitor, lastStart: visitor.now - 1_000 }), false);
  assert.equal(shouldAutoStart({ ...visitor, lastStart: visitor.now - LOOP_WINDOW_MS - 1 }), true);
  assert.equal(shouldAutoStart({ ...visitor, lastStart: visitor.now + 5_000 }), true, "a clock that moved back does not block");
});

test("the start address carries the page that the user asked for, encoded", () => {
  assert.equal(startUrl("/admin/auth/start", "/admin/Note/3", "?tab=a&x=1"), "/admin/auth/start?next=%2Fadmin%2FNote%2F3%3Ftab%3Da%26x%3D1");
  assert.equal(startUrl("/admin/auth/start", "/admin/", ""), "/admin/auth/start?next=%2Fadmin%2F");
});
