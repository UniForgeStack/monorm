import assert from "node:assert/strict";
import { test } from "node:test";

import { checkPassword, makePassword, needsUpdate } from "../../src/auth/index.js";
import { VECTORS } from "./support.js";

test("makePassword gives an argon2id hash with a new salt each time", () => {
  const a = makePassword("s3cret");
  const b = makePassword("s3cret");
  assert.ok(a.startsWith("$argon2id$v=19$m=65536,t=3,p=4$"));
  assert.notEqual(a, b);
  assert.ok(checkPassword("s3cret", a) && !checkPassword("other", a));
  assert.equal(needsUpdate(a), false);
});

test("the Django pbkdf2 vectors", () => {
  for (const c of VECTORS.pbkdf2) assert.equal(checkPassword(c.raw, c.encoded), c.valid, c.encoded);
});

test("the argon2id vectors", () => {
  for (const c of VECTORS.argon2id) {
    assert.equal(checkPassword(c.raw, c.encoded), c.valid, c.encoded);
    assert.equal(needsUpdate(c.encoded), c.needs_update, c.encoded);
  }
});

test("an unknown or broken hash is not a match", () => {
  for (const encoded of ["", "!", "$argon2id$broken", "plain", "bcrypt$x"]) assert.equal(checkPassword("s3cret", encoded), false, encoded);
});

test("a pbkdf2 hash needs an update", () => {
  assert.ok(needsUpdate(VECTORS.pbkdf2[0]!.encoded));
});
